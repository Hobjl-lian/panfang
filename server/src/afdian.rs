//! 爱发电订单同步：拉订单 → 按「账号绑定」给对应用户开通/收回权限。
//!
//! 与已核实的接口事实对应（2026-10-07 调研）：
//! - 查询订单：`POST https://ifdian.net/api/open/query-order`
//!   表单/JSON：`user_id`（创作者 ID）、`params`（JSON 字符串，如 `{"page":1}`）、`ts`（秒）、`sign`
//!   `sign = md5(token + "params" + params + "ts" + ts + "user_id" + user_id)`
//! - 订单字段：`out_trade_no`、`user_id`（**买家**）、`plan_id`、`sku_detail`、`total_amount`、`status`（2=交易成功）、`remark`
//!
//! 设计：
//! - **不依赖 OAuth**：绑定关系（爱发电用户 → 小米账号主体）由 OAuth 回调或管理员手动建立；
//!   订单同步只负责「谁买了什么 → 给他开通/收回」，两边解耦，OAuth 一批就能直接跑。
//! - 同一套 `apply_orders()` 既给真实订单用，也给「模拟一笔订单」（联调开关下开放）用。

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::{new_id, now};
use crate::state::ApiError;
use crate::state::{ApiResult, AppState};

pub const AFDIAN_API: &str = "https://ifdian.net/api/open/query-order";

/// OAuth2 关联授权（2026-10-07 实测：域名是 **afdian.com**）：
/// - ⚠️ `afdian.net` 已经注销（公共 DNS 都是 NXDOMAIN），旧文档里写的那些地址是死链；
///   换到 afdian.com 后两个地址都实测可达（授权页 200、换 token 端点回标准 OAuth 错误 JSON）。
/// - 授权页：`GET https://afdian.com/oauth2/authorize?client_id=&redirect_uri=&scope=basic&state=`
/// - 换身份：`POST https://afdian.com/api/oauth2/access_token`（表单 grant_type/client_id/client_secret/code/redirect_uri）
///   → 预期 `{ ec: 200, data: { user_id, name, avatar } }`，**没有 access_token**——只用来拿 user_id
/// - **仍待实测**（拿到 clientID 后必做）：scope=basic 是否够、redirect 白名单是否需报备、
///   换 token 的请求体到底是表单还是 JSON、返回里 user_id 的字段形态。
pub const AFDIAN_AUTHORIZE: &str = "https://afdian.com/oauth2/authorize";
pub const AFDIAN_TOKEN_API: &str = "https://afdian.com/api/oauth2/access_token";

/// OAuth 会话有效期（秒）：用户在浏览器里完成授权的窗口
pub const OAUTH_TTL: i64 = 15 * 60;

// ---------------------------------------------------------------- 签名

/// 爱发电要求的签名：`md5(token + "params" + params + "ts" + ts + "user_id" + user_id)`
pub fn sign(token: &str, params: &str, ts: i64, user_id: &str) -> String {
    let raw = format!("{token}params{params}ts{ts}user_id{user_id}");
    format!("{:x}", md5::compute(raw.as_bytes()))
}

// ---------------------------------------------------------------- 订单模型

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIn {
    pub out_trade_no: String,
    pub afdian_user_id: String,
    pub plan_id: String,
    /// 规格 id 列表（订单里的 sku_detail 会被拍平成一个字符串）
    #[serde(default)]
    pub sku: Option<String>,
    /// 规格**名**列表（爱发电里给规格起的名字，通常就是「小米手环10/小米手环11」这种设备名）
    #[serde(default)]
    pub sku_titles: Vec<String>,
    /// 2 = 交易成功
    pub status: i64,
    #[serde(default)]
    pub amount: f64,
    #[serde(default)]
    pub month: i64,
}

/// 爱发电的金额/月份等字段有时是字符串（"6.00"），数字与字符串都认
fn num_of(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse::<f64>().ok()))
        .unwrap_or(0.0)
}

/// 把订单接口返回的一条订单转成我们的模型（字段缺失不报错，交给业务判断）
pub fn parse_order(v: &Value) -> Option<OrderIn> {
    let out_trade_no = v["out_trade_no"].as_str()?.to_string();
    let afdian_user_id = v["user_id"].as_str()?.to_string();
    let plan_id = v["plan_id"].as_str().unwrap_or("").to_string();
    // sku_detail 形如 [{"sku_id":"xxxx","name":"小米手环10/小米手环11"}]，id 与名字分别拍平
    let sku_titles: Vec<String> = v["sku_detail"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["name"].as_str().map(|x| x.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let sku = v["sku_detail"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s["sku_id"].as_str().map(|x| x.to_string()))
                .collect::<Vec<_>>()
                .join(",")
        })
        .filter(|s| !s.is_empty());
    Some(OrderIn {
        out_trade_no,
        afdian_user_id,
        plan_id,
        sku,
        sku_titles,
        status: v["status"].as_i64().unwrap_or(-1),
        amount: num_of(&v["total_amount"]),
        month: v["month"].as_i64().unwrap_or(0),
    })
}

// ---------------------------------------------------------------- 规格名 ↔ 设备行
//
// 爱发电上「一个商品多个规格」很常见，规格名往往就是设备名（如「红米手表5/6」）。
// 只看商品编号会**误开通**：买的是红米那个规格，却把「手环10/11」那行也开了。
// 所以：行上若配了具体规格 id，就按 id 严格匹配；没配 id 时，用规格名去对行名。

/// 归一化设备名：去空白与各类分隔符、英文转小写；并把手环上的缩写补齐
/// （「小米手环10P」「小米手环9P&10P」里的 P 是 Pro，「小米手环10P」→「小米手环10pro」）
fn norm_device(s: &str) -> String {
    let base: String = s
        .chars()
        .filter(|c| !c.is_whitespace() && !"/、，,·・|＋+&【】()（）".contains(*c))
        .collect::<String>()
        .to_lowercase();
    let cs: Vec<char> = base.chars().collect();
    let mut out = String::with_capacity(base.len() + 4);
    for (i, c) in cs.iter().enumerate() {
        out.push(*c);
        if *c == 'p' {
            let after_digit = i > 0 && cs[i - 1].is_ascii_digit();
            let already_pro = cs.get(i + 1) == Some(&'r') && cs.get(i + 2) == Some(&'o');
            if after_digit && !already_pro {
                out.push_str("ro");
            }
        }
    }
    out
}

/// needle 是否作为「独立片段」出现在 hay 里：
/// 后面必须是结尾 / 数字 / 中文 / 符号——绝不能紧跟拉丁字母
/// （否则「小米手环10」会匹配上「小米手环10pro」，把普通版和 Pro 版搞混）。
fn covers(hay: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(i) = hay[start..].find(needle) {
        let abs = start + i;
        let rest = &hay[abs + needle.len()..];
        // 紧跟拉丁字母时：只有 "pro" 算"另一款设备"（10 vs 10 Pro），
        // 其余同族后缀放行（「红米手表5 eSIM」「小米手环10 NFC版」都还是这一族）
        let blocked = match rest.chars().next() {
            None => false,
            Some(c) if !c.is_ascii_alphabetic() => false,
            Some(_) => rest.starts_with("pro"),
        };
        if !blocked {
            return true;
        }
        start = abs + needle.len();
    }
    false
}

/// 规格名是否指向这个设备行。
/// 候选来自两处：行的机型列表（小米系是中文机型名）与行名片段（红米那行机型是英文，
/// 靠行名「红米手表5 / 6」来对）；任一候选中了即算命中。
pub fn sku_hits_row(sku_titles: &[String], row_name: &str, models: &[String]) -> bool {
    let mut candidates: Vec<String> = models.iter().map(|m| norm_device(m)).collect();
    // 行名按分隔符切段；只有数字/字母的段是缩写（「红米手表5 / 6」里的「6」），
    // 用第一段的前缀补全成「红米手表6」
    let segs: Vec<String> = row_name
        .split(['/', '、', '，', ',', '+', '&'])
        .map(norm_device)
        .filter(|p| !p.is_empty())
        .collect();
    let prefix = segs
        .first()
        .map(|s| s.trim_end_matches(|c: char| c.is_ascii_alphanumeric()).to_string())
        .unwrap_or_default();
    for seg in &segs {
        let has_cjk = seg.chars().any(|c| matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF));
        let full = if has_cjk || prefix.is_empty() {
            seg.clone()
        } else {
            format!("{prefix}{seg}")
        };
        if full.chars().count() >= 2 {
            candidates.push(full);
        }
    }
    candidates.retain(|c| !c.is_empty());
    if candidates.is_empty() {
        return false;
    }
    sku_titles.iter().any(|t| {
        let s = norm_device(t);
        !s.is_empty() && candidates.iter().any(|c| covers(&s, c))
    })
}

/// 行里的机型是 JSON 数组（["小米手环10","小米手环11"]）
pub fn models_of(json_str: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(json_str).unwrap_or_default()
}

// ---------------------------------------------------------------- 应用订单（核心，纯 DB，可单测）

#[derive(Debug, Default, Serialize)]
pub struct SyncSummary {
    pub seen: usize,
    pub new_grants: Vec<(String, String)>, // (mi_user_id, product_id)
    pub revoked: Vec<(String, String)>,
    /// 被拒绝的重放：(订单号, 目标 mi_user_id)。这笔订单的权益已归属别的账号，
    /// 换绑后同步又遇到它 → 拒绝。正常恒为空；非空 = 有人在换绑账号试图复制授权。
    pub replayed: Vec<(String, String)>,
    pub unbound: usize,   // 订单买家的爱发电账号还没绑定过 → 等他绑定后再同步
    pub unchanged: usize, // 重复订单 / 非成功状态且从未开通
}

/// 把订单应用到授权：绑定过就开通；退款/撤单就收回。**幂等**，可反复调用。
pub fn apply_orders(state: &AppState, orders: &[OrderIn]) -> ApiResult<SyncSummary> {
    let conn = state.conn();
    let mut sum = SyncSummary::default();

    for o in orders {
        sum.seen += 1;

        // 记录/更新订单（先落库，便于后台查看与对账）
        conn.execute(
            "INSERT INTO afdian_orders(out_trade_no, afdian_user_id, plan_id, sku, sku_name, status, amount, month, seen_at)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(out_trade_no) DO UPDATE SET status = excluded.status, seen_at = excluded.seen_at,
                     sku_name = COALESCE(excluded.sku_name, sku_name), sku = COALESCE(excluded.sku, sku)",
            params![
                o.out_trade_no,
                o.afdian_user_id,
                o.plan_id,
                o.sku,
                serde_json::to_string(&o.sku_titles).unwrap_or_else(|_| "[]".into()),
                o.status,
                o.amount,
                o.month,
                now()
            ],
        )?;

        // 这个买家绑定的是哪个小米账号？
        let binding: Option<(String, String)> = conn
            .query_row(
                "SELECT b.subject_id, s.mi_user_id FROM afdian_bindings b
                 JOIN subjects s ON s.id = b.subject_id WHERE b.afdian_user_id = ?1",
                [&o.afdian_user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;

        let source = format!("afdian:{}", o.out_trade_no);

        if o.status == 2 {
            let Some((subject_id, mi_user_id)) = binding else {
                sum.unbound += 1;
                log_line(format!(
                    "订单 {} 的买家 {} 还没绑定账号，等他绑定后再同步",
                    o.out_trade_no, o.afdian_user_id
                ));
                continue;
            };
            // 这笔订单的权益归属过某个账号吗？归属在**第一次处理**时钉下（见下），
            // 之后换绑到别的小米账号也不再重放 —— 否则同一订单可以在任意多个账号上
            // 各开通一遍，且旧账号的授权不会收回，等于一份订单可以复制给无限多人
            //（2026-10-16 修：买家靠「换绑 + 等一轮同步」就能把付费资源扩散出去）。
            let owner: Option<String> = conn
                .query_row(
                    "SELECT granted_subject_id FROM afdian_orders WHERE out_trade_no = ?1",
                    [&o.out_trade_no],
                    |r| r.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten();
            match owner {
                Some(owner_sid) if owner_sid != subject_id => {
                    sum.replayed.push((o.out_trade_no.clone(), mi_user_id.clone()));
                    log_line(format!(
                        "订单 {} 已归属账号 {}，拒绝重放给 {}（换绑不复制授权）",
                        o.out_trade_no, owner_sid, mi_user_id
                    ));
                    continue;
                }
                None => {
                    // 第一次处理：把权益归属钉在当前绑定的账号上。
                    // 即使这笔订单当下开不通（没配链接等），归属也已定下 —— 之后补链接仍开给它。
                    conn.execute(
                        "UPDATE afdian_orders SET granted_subject_id = ?2 WHERE out_trade_no = ?1",
                        params![o.out_trade_no, subject_id],
                    )?;
                }
                _ => {} // 归属就是当前账号：照常往下走（幂等）
            }
            // 这条订单能开通哪些资源：
            //   1) 行上配了 plan_id（购买链接里的商品编号）
            //   2) 行上配了 sku → 必须命中同一个规格 id
            //   3) 行上没配 sku → 用订单里的规格名去对行名（避免"买红米规格却开了手环那行"）
            //
            // ★ 2026-10-15 补的兜底（线上老订单实测得来）：爱发电侧**重建规格会换掉 sku id**，
            //   2025 年的 216 单（212 位买家）是受支持机型、商品里也有对应行，只是订单里的旧
            //   sku id 对不上今天的链接 → 严格 id 一条都没命中的时候，退一步把该商品的**全部**
            //   链接拿来按规格名对行。只在订单带规格名时才兜底（没名字无从判断是哪台设备）。
            struct Row {
                product_id: String,
                name: String,
                models: String,
                sku: Option<String>,
                /// 该行是被严格 sku id 命中的（命中即不再做规格名校验）
                id_hit: bool,
            }
            let fetch_rows = |by_id_only: bool| -> Result<Vec<Row>, rusqlite::Error> {
                let sql = if by_id_only {
                    "SELECT DISTINCT v.product_id, v.name, v.device_models, vl.sku
                     FROM variant_links vl JOIN variants v ON v.id = vl.variant_id
                     WHERE vl.plan_id = ?1
                       AND (vl.sku IS NULL OR vl.sku = ''
                            OR (?2 IS NOT NULL AND ?2 LIKE '%' || vl.sku || '%'))"
                } else {
                    "SELECT DISTINCT v.product_id, v.name, v.device_models, vl.sku
                     FROM variant_links vl JOIN variants v ON v.id = vl.variant_id
                     WHERE vl.plan_id = ?1"
                };
                // 两条 SQL 的占位符个数不同（2 个 / 1 个），参数按分支给
                let ps: Vec<&dyn rusqlite::ToSql> = if by_id_only {
                    vec![&o.plan_id, &o.sku]
                } else {
                    vec![&o.plan_id]
                };
                let mut stmt = conn.prepare(sql)?;
                let rows: Vec<Row> = stmt
                    .query_map(ps.as_slice(), |r| {
                        Ok(Row {
                            product_id: r.get(0)?,
                            name: r.get(1)?,
                            models: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                            sku: r.get::<_, Option<String>>(3)?.filter(|s| !s.trim().is_empty()),
                            id_hit: by_id_only,
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                Ok(rows)
            };
            let mut rows = fetch_rows(true)?;
            if rows.is_empty() && !o.sku_titles.is_empty() {
                rows = fetch_rows(false)?;
                if !rows.is_empty() {
                    log_line(format!(
                        "订单 {} 的规格 id 已失效（sku={:?}），改按规格名 {:?} 对行",
                        o.out_trade_no, o.sku, o.sku_titles
                    ));
                }
            }
            if rows.is_empty() {
                log_line(format!(
                    "订单 {} 的商品（plan_id={} sku={:?}）没有对应到任何资源——管理页里补一下购买链接",
                    o.out_trade_no, o.plan_id, o.sku
                ));
                continue;
            }
            let mut products: Vec<String> = Vec::new();
            let mut skipped: Vec<String> = Vec::new();
            for row in &rows {
                // 严格 id 命中的行直接信 id；没配 sku 的行、以及兜底路径拿到的行，都按规格名对
                let by_id = row.id_hit && row.sku.is_some();
                if !by_id
                    && !o.sku_titles.is_empty()
                    && !sku_hits_row(&o.sku_titles, &row.name, &models_of(&row.models))
                {
                    skipped.push(row.name.clone());
                    continue;
                }
                if !products.contains(&row.product_id) {
                    products.push(row.product_id.clone());
                }
            }
            if products.is_empty() {
                log_line(format!(
                    "订单 {} 的规格（{:?}）对不上任何设备行——检查爱发电规格名与管理页里的行名（行名示例：{:?}）",
                    o.out_trade_no,
                    o.sku_titles,
                    rows.iter().map(|r| r.name.clone()).collect::<Vec<_>>()
                ));
                sum.unchanged += 1;
                continue;
            }
            if !skipped.is_empty() {
                log_line(format!(
                    "订单 {} 的规格（{:?}）与设备行 {:?} 不符，已跳过这几行",
                    o.out_trade_no, o.sku_titles, skipped
                ));
            }
            let mut granted_any = false;
            for pid in &products {
                let n = conn.execute(
                    // 幂等：只有「原本不是 active」才会真的改到行（changes() > 0），
                    // 重复同步同一订单不会重复计入开通
                    "INSERT INTO licenses(subject_id, product_id, source, status, created_at)
                     VALUES(?1, ?2, ?3, 'active', ?4)
                     ON CONFLICT(subject_id, product_id) DO UPDATE SET status = 'active', revoked_at = NULL,
                         source = excluded.source   -- 记下最后一次开通它的订单，退款时据此收回
                     WHERE licenses.status != 'active'",
                    params![subject_id, pid, source, now()],
                )?;
                // n=0 表示原本就是 active（重复同步）
                if n > 0 {
                    sum.new_grants.push((mi_user_id.clone(), pid.clone()));
                    granted_any = true;
                }
            }
            if granted_any {
                conn.execute(
                    "UPDATE afdian_orders SET granted_at = ?2 WHERE out_trade_no = ?1",
                    params![o.out_trade_no, now()],
                )?;
                log_line(format!(
                    "订单 {} → 给 {} 开通 {:?}",
                    o.out_trade_no, mi_user_id, products
                ));
            } else {
                // 资源早就在有效期内（历史手动授权、或别的订单已开过）→ 没有新开通。
                // ⚠️ 这里原本完全静默，2026-10-15 排障时被它绕了半小时，所以留一行日志。
                sum.unchanged += 1;
                log_line(format!(
                    "订单 {} 的资源已在有效期内（{:?}），无需重复开通",
                    o.out_trade_no, products
                ));
            }
        } else {
            // 非成功状态（未支付 / 已退款等）：如果之前给这条订单开通过，就收回
            let granted_before: i64 = conn.query_row(
                "SELECT COUNT(*) FROM afdian_orders WHERE out_trade_no = ?1 AND granted_at IS NOT NULL",
                [&o.out_trade_no],
                |r| r.get(0),
            )?;
            if granted_before == 0 {
                sum.unchanged += 1;
                continue;
            }
            let n = conn.execute(
                "UPDATE licenses SET status='revoked', revoked_at=?2 WHERE source = ?1 AND status='active'",
                params![source, now()],
            )?;
            if n > 0 {
                if let Some((_, mi_user_id)) = binding {
                    sum.revoked.push((mi_user_id, o.plan_id.clone()));
                }
                log_line(format!(
                    "订单 {} 状态变为 {}（退款/撤单）→ 收回 {} 个授权",
                    o.out_trade_no, o.status, n
                ));
            }
        }
    }
    Ok(sum)
}

/// 给某资源「补发」历史订单：绑定后回头把之前的订单一次性同步上（幂等）
pub fn sync_after_bind(state: &AppState, afdian_user_id: &str) -> ApiResult<usize> {
    // ⚠️ 取数与写库分开：`apply_orders` 自己会抢锁，这里绝不能持有 guard 再调它
    //（std::sync::Mutex 不可重入，2026-10-07 已踩过两次）
    let orders: Vec<OrderIn> = {
        let conn = state.conn();
        let bound: Option<String> = conn
            .query_row(
                "SELECT subject_id FROM afdian_bindings WHERE afdian_user_id = ?1",
                [afdian_user_id],
                |r| r.get(0),
            )
            .optional()?;
        if bound.is_none() {
            return Ok(0);
        }
        // 该买家所有成功、且还没开通过的订单
        let mut stmt = conn.prepare(
            "SELECT out_trade_no, afdian_user_id, plan_id, sku, sku_name, status, amount, month
             FROM afdian_orders WHERE afdian_user_id = ?1 AND status = 2 AND granted_at IS NULL",
        )?;
        let v: Vec<OrderIn> = stmt
            .query_map([afdian_user_id], |r| {
                Ok(OrderIn {
                    out_trade_no: r.get(0)?,
                    afdian_user_id: r.get(1)?,
                    plan_id: r.get(2)?,
                    sku: r.get(3)?,
                    sku_titles: r
                        .get::<_, Option<String>>(4)?
                        .and_then(|j| serde_json::from_str(&j).ok())
                        .unwrap_or_default(),
                    status: r.get(5)?,
                    amount: r.get(6)?,
                    month: r.get(7)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        v
    };
    let n = orders.len();
    apply_orders(state, &orders)?;
    Ok(n)
}

fn log_line(s: String) {
    println!("[afdian] {s}");
}

// ---------------------------------------------------------------- 绑定关系

/// 建立「爱发电账号 → 小米账号主体」绑定（OAuth 回调用；管理员手动也能用）
pub fn bind_afdian(state: &AppState, afdian_user_id: &str, mi_user_id: &str) -> ApiResult<String> {
    let conn = state.conn();
    let subject_id: String = match conn
        .query_row("SELECT id FROM subjects WHERE mi_user_id = ?1", [mi_user_id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
    {
        Some(id) => id,
        None => {
            let id = new_id("s_");
            conn.execute(
                "INSERT INTO subjects(id, mi_user_id, created_at) VALUES(?1, ?2, ?3)",
                params![id, mi_user_id, now()],
            )?;
            id
        }
    };
    conn.execute(
        "INSERT INTO afdian_bindings(afdian_user_id, subject_id, bound_at) VALUES(?1, ?2, ?3)
         ON CONFLICT(afdian_user_id) DO UPDATE SET subject_id = excluded.subject_id, bound_at = excluded.bound_at",
        params![afdian_user_id, subject_id, now()],
    )?;
    Ok(subject_id)
}

// ---------------------------------------------------------------- 拉取订单（网络）

/// 调爱发电接口拉一页订单；未配置令牌时返回错误（后台会自动跳过）
pub async fn query_orders_page(page: i64) -> Result<Vec<OrderIn>> {
    let user_id = std::env::var("WF_AFDIAN_USER_ID").unwrap_or_default().trim().to_string();
    let token = std::env::var("WF_AFDIAN_TOKEN").unwrap_or_default().trim().to_string();
    if user_id.is_empty() || token.is_empty() {
        bail!("未配置 WF_AFDIAN_USER_ID / WF_AFDIAN_TOKEN");
    }
    let params_json = json!({ "page": page }).to_string();
    let ts = now();
    let sign = sign(&token, &params_json, ts, &user_id);
    let body = json!({
        "user_id": user_id,
        "params": params_json,
        "ts": ts,
        "sign": sign,
    });
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(AFDIAN_API)
        .json(&body)
        .send()
        .await
        .context("请求爱发电失败")?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        bail!("爱发电返回 HTTP {}：{}", status.as_u16(), v);
    }
    if v["ec"].as_i64() != Some(200) {
        bail!("爱发电接口报错：{}", v["em"].as_str().unwrap_or("未知"));
    }
    let list = v["data"]["list"].as_array().cloned().unwrap_or_default();
    let orders: Vec<OrderIn> = list.iter().filter_map(parse_order).collect();
    Ok(orders)
}

/// 同步一轮：拉最近 N 页 + 应用（幂等）
pub async fn sync_once(state: &AppState, pages: i64) -> Result<(SyncSummary, usize)> {
    let mut all = Vec::new();
    for p in 1..=pages.max(1) {
        let page = query_orders_page(p).await?;
        let n = page.len();
        all.extend(page);
        if n == 0 {
            break;
        }
    }
    let count = all.len();
    let sum = apply_orders(state, &all).map_err(|e| anyhow!("{e:?}"))?;
    Ok((sum, count))
}

// ---------------------------------------------------------------- OAuth2 关联授权

/// OAuth 应用凭据（审批通过后填进 /etc/wf-auth.env 即可生效）
pub struct OauthCfg {
    pub client_id: String,
    pub secret: String,
    pub redirect: String,
}

fn env_trim(k: &str) -> String {
    std::env::var(k).unwrap_or_default().trim().to_string()
}

/// 是否已配好 OAuth：没配就返回 None（绑定页会显示「暂不可用」）
pub fn oauth_cfg() -> Option<OauthCfg> {
    let client_id = env_trim("WF_AFDIAN_CLIENT_ID");
    let secret = env_trim("WF_AFDIAN_CLIENT_SECRET");
    if client_id.is_empty() || secret.is_empty() {
        return None;
    }
    let base = env_trim("WF_PUBLIC_BASE");
    let default_redirect = if base.is_empty() {
        "https://api.hobjl-shop.top/v1/afdian/callback".to_string()
    } else {
        format!("{}/v1/afdian/callback", base.trim_end_matches('/'))
    };
    let redirect = match env_trim("WF_AFDIAN_REDIRECT") {
        s if s.is_empty() => default_redirect,
        s => s,
    };
    Some(OauthCfg { client_id, secret, redirect })
}

/// 最小百分号编码（授权 URL 里只有 client_id / redirect_uri / state 三个值）
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 联调开关（服务端 `WF_DEV_BIND=1` **且** `WF_DEV_BIND_UNTIL` 未过期）。
///
/// **故意做成 fail-closed**：只写 `WF_DEV_BIND=1` 而没写到期时间 = 视为关闭。
/// 因为「忘关」是这个开关唯一的真实风险（开着时 `/v1/bind/dev` 会给任何人开通全部资源），
/// 到期时间让它自己关上，不需要任何人记得。
pub fn dev_mode() -> bool {
    if std::env::var("WF_DEV_BIND").ok().as_deref() != Some("1") {
        return false;
    }
    matches!(dev_until(), Some(t) if t > crate::db::now())
}

/// 联调到期时间（unix 秒，环境变量 `WF_DEV_BIND_UNTIL`）
pub fn dev_until() -> Option<i64> {
    std::env::var("WF_DEV_BIND_UNTIL")
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
}

/// 给启动日志 / 管理页看的联调状态（含原因与人类可读的到期时间）
pub fn dev_status() -> serde_json::Value {
    let wanted = std::env::var("WF_DEV_BIND").ok().as_deref() == Some("1");
    let until = dev_until();
    let on = dev_mode();
    let why = if !wanted {
        "未开启（没有 WF_DEV_BIND=1）"
    } else if until.is_none() {
        "已按关闭处理：只设了 WF_DEV_BIND=1，没设到期时间 WF_DEV_BIND_UNTIL"
    } else if !on {
        "已到期（自动关闭）"
    } else {
        "开启中"
    };
    json!({
        "wanted": wanted,
        "on": on,
        "until": until,
        "until_text": until.map(human_time).unwrap_or_default(),
        "why": why,
    })
}

/// unix 秒 → 「2026-10-21 15:04」本地字样（只用于展示；服务器时区即本地时区）
fn human_time(ts: i64) -> String {
    // 不引 chrono：自己按 UTC+8 折算成可读字符串，够用且不出错
    let secs = ts + 8 * 3600;
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}（北京时间）",
        rem / 3600,
        (rem % 3600) / 60
    )
}

/// 把一个「1970 起的天数」换成公历年月日（Howard Hinnant 的算法）
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 给用户跳转的授权页地址。
/// 联调模式（WF_DEV_BIND=1）下指向我们自己的「模拟授权页」，审批通过后自动切回爱发电真实授权页。
pub fn authorize_url(cfg: &OauthCfg, state: &str) -> String {
    let base = env_trim("WF_PUBLIC_BASE");
    if dev_mode() && !base.is_empty() {
        return format!(
            "{}/v1/afdian/dev-authorize?state={}",
            base.trim_end_matches('/'),
            encode(state)
        );
    }
    format!(
        "{}?client_id={}&redirect_uri={}&scope=basic&state={}",
        AFDIAN_AUTHORIZE,
        encode(&cfg.client_id),
        encode(&cfg.redirect),
        encode(state)
    )
}

/// 联调用：`WF_DEV_BIND=1` 时，回调带 `code=dev:<爱发电用户ID>` 就直接当成已授权的该账号，
/// 不真的去爱发电换身份——这样 OAuth 审批之前，整条「打开授权页 → 回调 → App 轮询 → 自动绑定」也能自测。
pub fn dev_code_uid(code: &str) -> Option<String> {
    if !dev_mode() {
        return None;
    }
    let uid = code.strip_prefix("dev:")?.trim();
    if uid.is_empty() {
        None
    } else {
        Some(uid.to_string())
    }
}

/// 用授权码换身份，返回爱发电 user_id
pub async fn exchange_code(cfg: &OauthCfg, code: &str) -> Result<String> {
    let form = [
        ("grant_type", "authorization_code".to_string()),
        ("client_id", cfg.client_id.clone()),
        ("client_secret", cfg.secret.clone()),
        ("code", code.to_string()),
        ("redirect_uri", cfg.redirect.clone()),
    ];
    let resp = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .post(AFDIAN_TOKEN_API)
        .form(&form)
        .send()
        .await
        .context("请求爱发电换取身份失败")?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or_else(|_| json!({}));
    if !status.is_success() {
        bail!("爱发电返回 HTTP {}：{}", status.as_u16(), v);
    }
    if v["ec"].as_i64() != Some(200) {
        bail!("爱发电授权失败：{}", v["em"].as_str().unwrap_or("未知错误"));
    }
    // user_id 有的是字符串、有的是数字，两种都认
    let uid = v["data"]["user_id"]
        .as_str()
        .map(|s| s.to_string())
        .or_else(|| v["data"]["user_id"].as_i64().map(|n| n.to_string()))
        .unwrap_or_default();
    if uid.is_empty() {
        bail!("爱发电返回里没有 user_id");
    }
    Ok(uid)
}

/// 全量拉取：从第 1 页一直翻到空页（迁移老用户时把历史订单一次灌进库）。
/// `progress` 每 10 页回调一次，便于打日志/看进度。
pub async fn sync_all(
    state: &AppState,
    progress: impl Fn(i64, usize),
) -> Result<(SyncSummary, usize)> {
    let mut all: Vec<OrderIn> = Vec::new();
    let mut page = 1i64;
    // 安全上限：2000 页（爱发电 50 单/页 → 10 万单）
    while page <= 2000 {
        let list = query_orders_page(page).await?;
        if list.is_empty() {
            break;
        }
        let n = list.len();
        all.extend(list);
        if page % 10 == 0 {
            progress(page, all.len());
        }
        page += 1;
        if n < 20 {
            // 页面没满，一般就是最后一页
            break;
        }
    }
    if page > 2000 {
        println!("[afdian] 警告：订单超过 10 万，这轮只拉到第 2000 页");
    }
    let count = all.len();
    let sum = apply_orders(state, &all).map_err(|e| anyhow!("{e:?}"))?;
    Ok((sum, count))
}

/// 规格名体检：**每个商品**（plan_id）的订单里出现过的规格名，能不能落到某个设备行。
/// 老用户迁移前先跑：✗ 的规格名 → 那批买家绑定了也拿不到权限（要补链接或改名/加行）。
pub fn spec_report(state: &AppState) -> ApiResult<Vec<Value>> {
    let conn = state.conn();
    // 所有已配链接：行 ↔ 商品
    let links: Vec<(String, String, String)> = {
        let mut stmt = conn.prepare(
            "SELECT vl.plan_id, v.name, v.device_models
             FROM variant_links vl JOIN variants v ON v.id = vl.variant_id",
        )?;
        let rows: Vec<(String, String, String)> = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .filter_map(|r| r.ok())
            .collect();
        rows
    };
    // 按商品聚合：这个商品配了哪些行
    let mut plans: Vec<(String, Vec<(String, Vec<String>)>)> = Vec::new();
    for (plan, vname, models_json) in links {
        let entry = match plans.iter_mut().find(|(p, _)| *p == plan) {
            Some(e) => e,
            None => {
                plans.push((plan.clone(), Vec::new()));
                plans.last_mut().unwrap()
            }
        };
        entry.1.push((vname, models_of(&models_json)));
    }

    let mut out = Vec::new();
    for (plan, rows) in plans {
        // 订单里出现过的规格名（含未绑定买家）
        let names: Vec<String> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT sku_name FROM afdian_orders
                 WHERE plan_id = ?1 AND status = 2 AND sku_name IS NOT NULL",
            )?;
            let raw: Vec<String> = stmt
                .query_map([&plan], |r| r.get::<_, String>(0))?
                .filter_map(|r| r.ok())
                .collect();
            let mut set: Vec<String> = Vec::new();
            for j in raw {
                if let Ok(v) = serde_json::from_str::<Vec<String>>(&j) {
                    for x in v {
                        if !set.contains(&x) {
                            set.push(x);
                        }
                    }
                }
            }
            set.sort();
            set
        };
        let specs: Vec<Value> = names
            .iter()
            .map(|n| {
                let to: Vec<String> = rows
                    .iter()
                    .filter(|(vname, models)| sku_hits_row(std::slice::from_ref(n), vname, models))
                    .map(|(vname, _)| vname.clone())
                    .collect();
                json!({ "name": n, "ok": !to.is_empty(), "to": to })
            })
            .collect();
        let buyers: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT afdian_user_id) FROM afdian_orders WHERE plan_id = ?1 AND status = 2",
                [&plan],
                |r| r.get(0),
            )
            .unwrap_or(0);
        out.push(json!({
            "planId": plan,
            "rows": rows.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
            "buyers": buyers,
            "specs": specs,
        }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_time_matches_beijing_clock() {
        // 联调警示条要显示「什么时候自动关闭」，算错日期会让人误判发布窗口
        assert_eq!(human_time(1792566240), "2026-10-21 15:04（北京时间）");
        assert_eq!(human_time(1798732800), "2027-01-01 00:00（北京时间）");
        assert_eq!(human_time(1772328600), "2026-03-01 09:30（北京时间）");
    }

    #[test]
    fn sign_matches_documented_formula() {
        // md5(token + "params" + params + "ts" + ts + "user_id" + user_id)
        let s = sign("TK", "{\"page\":1}", 1700000000, "UID");
        let expect = format!("{:x}", md5::compute(b"TKparams{\"page\":1}ts1700000000user_idUID"));
        assert_eq!(s, expect);
    }

    #[test]
    fn authorize_url_encodes_redirect() {
        let cfg = OauthCfg {
            client_id: "cid-1".into(),
            secret: "s".into(),
            redirect: "https://api.hobjl-shop.top/v1/afdian/callback".into(),
        };
        let u = authorize_url(&cfg, "STATE123");
        assert!(u.starts_with(AFDIAN_AUTHORIZE), "{u}");
        assert!(u.contains("client_id=cid-1"), "{u}");
        assert!(u.contains("scope=basic"), "{u}");
        assert!(u.contains("state=STATE123"), "{u}");
        // 回调地址里的 :// 必须被编码，否则甲方解析会截断
        assert!(u.contains("redirect_uri=https%3A%2F%2Fapi.hobjl-shop.top%2Fv1%2Fafdian%2Fcallback"), "{u}");
    }

    #[test]
    fn parse_order_flattens_sku() {
        let v = json!({
            "out_trade_no": "20261007ABC",
            "user_id": "buyer-1",
            "plan_id": "plan-x",
            "status": 2,
            "total_amount": "6.00",
            "sku_detail": [
                {"sku_id": "sku_9", "name": "小米手环9"},
                {"sku_id": "sku_10", "name": "小米手环10/小米手环11"}
            ]
        });
        let o = parse_order(&v).unwrap();
        assert_eq!(o.out_trade_no, "20261007ABC");
        assert_eq!(o.afdian_user_id, "buyer-1");
        assert_eq!(o.sku.as_deref(), Some("sku_9,sku_10"));
        assert_eq!(o.sku_titles, vec!["小米手环9", "小米手环10/小米手环11"]);
        assert_eq!(o.status, 2);
        assert_eq!(o.amount, 6.0);
    }

    /// 用**用户店铺里的真实规格名**做回归：一个商品多规格时，只能开通对应的设备行。
    /// （规格名样例取自 2026-10-07 线上拉到的 100 笔真实订单）
    #[test]
    fn sku_title_routes_to_the_right_row() {
        let t = |s: &str| vec![s.to_string()];
        let band10 = vec!["小米手环10".to_string(), "小米手环11".to_string()];
        let pro_row = vec!["小米手环9 Pro".to_string(), "小米手环10 Pro".to_string()];
        let band9 = vec!["小米手环9".to_string()];
        let redmi = vec![
            "Redmi Watch 5".to_string(),
            "Redmi Watch 5 eSIM".to_string(),
            "Redmi Watch 6".to_string(),
        ];

        // —— 「小米手环10 / 11」行：手环10、11 的各种写法都认
        for sku in ["小米手环10/小米手环11", "小米手环10", "小米手环11"] {
            assert!(sku_hits_row(&t(sku), "小米手环10 / 11", &band10), "{sku}");
        }
        // 但 Pro 版、9 代不能误开
        for sku in ["小米手环10P", "【新适配】小米手环10P", "小米手环9P&10P", "小米手环9"] {
            assert!(!sku_hits_row(&t(sku), "小米手环10 / 11", &band10), "{sku}");
        }

        // —— 「小米手环9 Pro / 10 Pro」行
        for sku in [
            "小米手环10P",
            "小米手环9P",
            "【新适配】小米手环10P",
            "小米手环9P&10P",
            "小米手环9Pro/小米手环10Pro",
        ] {
            assert!(sku_hits_row(&t(sku), "小米手环9 Pro / 10 Pro", &pro_row), "{sku}");
        }
        for sku in ["小米手环10/小米手环11", "小米手环9", "小米手环11"] {
            assert!(!sku_hits_row(&t(sku), "小米手环9 Pro / 10 Pro", &pro_row), "{sku}");
        }

        // —— 「小红米手表5 / 6」行（机型是英文，靠行名对中文规格名）
        for sku in ["红米手表5/6", "红米手表5/红米手表6", "红米手表5&6", "红米手表5", "红米手表6", "红米手表5 eSIM"] {
            assert!(sku_hits_row(&t(sku), "红米手表5 / 6", &redmi), "{sku}");
        }
        assert!(!sku_hits_row(&t("小米手环10/小米手环11"), "红米手表5 / 6", &redmi));

        // —— 单机型行：只认完全同名，不被 Pro 版蹭到
        assert!(sku_hits_row(&t("小米手环9"), "小米手环9", &band9));
        assert!(!sku_hits_row(&t("小米手环9P"), "小米手环9", &band9));
        assert!(!sku_hits_row(&t("红米手表6"), "小米手环9", &band9));
    }

    /// 老订单兜底的**名字路由**：爱发电重建规格会换掉 sku id，2025 年的 216 单
    /// （212 位买家）就是「id 对不上、但名字还认得出是哪台设备」。
    /// 这里锁的是那批订单里出现过的全部规格名（线上实测分布：手环10 197 / 红米手表5 14 / 红米手表5/6 5）。
    #[test]
    fn stale_sku_names_still_route_to_the_right_row() {
        let t = |s: &str| vec![s.to_string()];
        let band10 = vec!["小米手环10".to_string(), "小米手环11".to_string()];
        let pro_row = vec!["小米手环9 Pro".to_string(), "小米手环10 Pro".to_string()];
        let redmi = vec![
            "Redmi Watch 5".to_string(),
            "Redmi Watch 5 eSIM".to_string(),
            "Redmi Watch 6".to_string(),
        ];
        for sku in ["小米手环10", "红米手表5", "红米手表5/6"] {
            let hit_band10 = sku_hits_row(&t(sku), "小米手环10 / 11", &band10);
            let hit_redmi = sku_hits_row(&t(sku), "红米手表5 / 6", &redmi);
            assert!(hit_band10 ^ hit_redmi, "{sku} 应恰好命中一行");
            assert!(!sku_hits_row(&t(sku), "小米手环9 Pro / 10 Pro", &pro_row), "{sku} 不能蹭到 Pro 行");
        }
        // 兜底不代表乱开：8Pro / 红米4 这些没有行的老设备照样落不到任何行
        for sku in ["小米手环8P", "小米手环8Pro", "红米手表4", "红米4"] {
            assert!(!sku_hits_row(&t(sku), "小米手环10 / 11", &band10), "{sku}");
            assert!(!sku_hits_row(&t(sku), "红米手表5 / 6", &redmi), "{sku}");
        }
    }

    /// 数据库级回归：**规格 id 失效**的老订单要按规格名兜底开通；没对应行的规格（8Pro）不许乱开；
    /// 严格 id 命中时仍不做名字校验（老口径不变）。
    /// （上一版就是这条路径上写错了 SQL 占位符 → 线上 500；纯函数测试抓不到，必须有这条。）
    #[test]
    fn stale_sku_order_grants_via_name_fallback() {
        use crate::state::AppState;
        use std::sync::{Arc, Mutex};

        let dir = std::env::temp_dir().join(format!("wf-afdian-test-{}", crate::db::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::open(&dir.join("wf.db")).unwrap();
        for (pid, vid, plan) in [("p1", "v1", "planA"), ("p2", "v2", "planB")] {
            conn.execute(
                "INSERT INTO products(id, name, status, created_at) VALUES(?1, ?1, 'on', 0)",
                [pid],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO variants(id, product_id, name, device_models, created_at)
                 VALUES(?1, ?2, '小米手环10 / 11', '[\"小米手环10\",\"小米手环11\"]', 0)",
                [vid, pid],
            )
            .unwrap();
            // 链接上是**今天的** sku id（老订单带的是旧 id）
            conn.execute(
                "INSERT INTO variant_links(variant_id, url, plan_id, sku, created_at)
                 VALUES(?1, 'https://afdian.com/a/x', ?2, 'NEWSKU', 0)",
                [vid, plan],
            )
            .unwrap();
        }
        let state = AppState {
            db: Arc::new(Mutex::new(conn)),
            data_dir: dir.clone(),
            cipher_dir: dir.clone(),
            kek: [0u8; 32],
            url_secret: vec![0u8; 32],
            admin_token: "t".into(),
            public_base: "http://test".into(),
            device_limit: 2,
            device_stale_secs: 7 * 86400,
            token_ttl: 100,
        };
        bind_afdian(&state, "afdian-buyer-1", "mi-test-1").unwrap();

        let mk = |no: &str, plan: &str, sku: &str, name: &str| OrderIn {
            out_trade_no: no.into(),
            afdian_user_id: "afdian-buyer-1".into(),
            plan_id: plan.into(),
            sku: Some(sku.into()),
            sku_titles: vec![name.into()],
            status: 2,
            amount: 6.0,
            month: 0,
        };

        // 老 sku id + 规格名在 → 兜底开通
        let sum = apply_orders(&state, &[mk("OLD1", "planA", "OLDSKU", "小米手环10")]).unwrap();
        assert_eq!(sum.new_grants.len(), 1, "老规格 id 应按名字兜底开通");
        // 幂等：再来一遍不重复开通
        let sum = apply_orders(&state, &[mk("OLD1", "planA", "OLDSKU", "小米手环10")]).unwrap();
        assert_eq!(sum.new_grants.len(), 0);
        assert_eq!(sum.unchanged, 1);
        // 名字对不上任何行（8Pro）→ 不开
        let sum = apply_orders(&state, &[mk("OLD2", "planA", "OLDSKU8", "小米手环8Pro")]).unwrap();
        assert_eq!(sum.new_grants.len(), 0);
        assert_eq!(sum.unchanged, 1);
        // 严格 id 命中时不看名字（老口径：id 对得上就开）
        let sum = apply_orders(&state, &[mk("NEW1", "planB", "NEWSKU", "随便写的")]).unwrap();
        assert_eq!(sum.new_grants.len(), 1, "id 命中应直接开通");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 换绑不复制授权：同一笔订单只能开通给一个主体（2026-10-16 修的洞）。
    ///
    /// 漏洞形态：买家把爱发电账号换绑到朋友的小米账号后，下一轮订单同步会把同一订单
    /// 再开通一遍，且旧账号的授权不收回 —— 一份订单可扩散到无限多个账号。
    /// 现在归属在首次处理时钉在 `afdian_orders.granted_subject_id` 上，重放会被拒绝。
    #[test]
    fn rebind_does_not_replay_order_to_new_subject() {
        use crate::state::AppState;
        use std::sync::{Arc, Mutex};

        let dir = std::env::temp_dir().join(format!("wf-afdian-rebind-{}", crate::db::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        let conn = crate::db::open(&dir.join("wf.db")).unwrap();
        conn.execute(
            "INSERT INTO products(id, name, status, created_at) VALUES('p1', '测试表盘', 'on', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO variants(id, product_id, name, device_models, created_at)
             VALUES('v1', 'p1', '手环10/11', '[\"小米手环10\"]', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO variant_links(variant_id, url, plan_id, sku, created_at)
             VALUES('v1', 'https://afdian.com/a/x', 'planA', NULL, 0)",
            [],
        )
        .unwrap();
        let state = AppState {
            db: Arc::new(Mutex::new(conn)),
            data_dir: dir.clone(),
            cipher_dir: dir.clone(),
            kek: [0u8; 32],
            url_secret: vec![0u8; 32],
            admin_token: "t".into(),
            public_base: "http://test".into(),
            device_limit: 2,
            device_stale_secs: 7 * 86400,
            token_ttl: 100,
        };
        let subj_of = |mi: &str| -> String {
            let conn = state.conn();
            conn.query_row("SELECT id FROM subjects WHERE mi_user_id = ?1", [mi], |r| r.get(0))
                .unwrap()
        };

        // 买家绑 A → 订单开通给 A
        bind_afdian(&state, "U1", "miA").unwrap();
        let order = OrderIn {
            out_trade_no: "X1".into(),
            afdian_user_id: "U1".into(),
            plan_id: "planA".into(),
            sku: None,
            sku_titles: vec![], // 不带规格名：链接行上也没配 sku → 直接命中
            status: 2,
            amount: 6.0,
            month: 0,
        };
        let s1 = apply_orders(&state, &[order.clone()]).unwrap();
        assert_eq!(s1.new_grants.len(), 1, "首次应开通给 A");
        let a = subj_of("miA");
        {
            let conn = state.conn();
            let owner: Option<String> = conn
                .query_row(
                    "SELECT granted_subject_id FROM afdian_orders WHERE out_trade_no = 'X1'",
                    [],
                    |r| r.get::<_, Option<String>>(0),
                )
                .unwrap();
            assert_eq!(owner.as_deref(), Some(a.as_str()), "订单归属应钉在 A");
        }
        // 幂等：同一订单重复同步不重复开通、不算重放
        let s2 = apply_orders(&state, &[order.clone()]).unwrap();
        assert!(s2.new_grants.is_empty() && s2.replayed.is_empty());

        // 换绑到 B（把爱发电账号改绑到另一个小米账号）→ 同步重放必须被拒绝
        bind_afdian(&state, "U1", "miB").unwrap();
        let s3 = apply_orders(&state, &[order.clone()]).unwrap();
        assert!(s3.new_grants.is_empty(), "换绑后不得再开通");
        assert_eq!(s3.replayed, vec![("X1".to_string(), "miB".to_string())]);
        let b = subj_of("miB");
        {
            let conn = state.conn();
            let nb: i64 = conn
                .query_row("SELECT COUNT(*) FROM licenses WHERE subject_id = ?1", [&b], |r| r.get(0))
                .unwrap();
            assert_eq!(nb, 0, "B 不应获得任何授权");
            let na: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM licenses WHERE subject_id = ?1 AND status = 'active'",
                    [&a],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(na, 1, "A 的授权应保持");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
