//! 密钥体系（契约 §0.2 冻结参数，不可擅自更换）：
//! KEK（主密钥，服务端持有）→ CEK（每文件 AES-256-GCM）→ 设备公钥 ECIES 包裹下发。

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use anyhow::{anyhow, bail, Context, Result};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::{Digest, Sha256};
use std::path::Path;
use x25519_dalek::{PublicKey, StaticSecret};

pub const WRAP_INFO: &[u8] = b"wf-cek-wrap-v1";

// ---------------------------------------------------------------- 随机与编码

pub fn rand_bytes(n: usize) -> Vec<u8> {
    let mut v = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut v);
    v
}

pub fn b64(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(data)
}

pub fn b64d(s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s.trim().as_bytes())
        .context("base64 解码失败")
}

pub fn b64url(data: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

// ---------------------------------------------------------------- 主密钥（KEK）

/// **只读**主密钥：`WF_KEK`（64 位 hex）优先，否则 `<data_dir>/kek.bin`；没有就报错。
///
/// 离线工具（backfill 之类）必须用这个：现场生成一把新的主密钥＝解不开任何已有密文，
/// 只会让人误以为「数据没了」。
pub fn read_kek(data_dir: &Path) -> Result<[u8; 32]> {
    if let Ok(s) = std::env::var("WF_KEK") {
        return kek_from_hex(&s);
    }
    read_kek_at(&data_dir.join("kek.bin"))
}

/// 从明确给定的文件读主密钥（调用方负责确认这个路径没越界）。
pub fn read_kek_at(path: &Path) -> Result<[u8; 32]> {
    if let Ok(s) = std::env::var("WF_KEK") {
        return kek_from_hex(&s);
    }
    if !path.exists() {
        bail!(
            "{} 不存在，也没有 WF_KEK —— 解不开密文。用 WF_DATA_DIR 指到正确的数据目录",
            path.display()
        );
    }
    let raw = std::fs::read(path)?;
    if raw.len() != 32 {
        bail!("{} 内容不是 32 字节", path.display());
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&raw);
    Ok(k)
}

fn kek_from_hex(s: &str) -> Result<[u8; 32]> {
    let raw = hex::decode(s.trim()).context("WF_KEK 需要 64 位十六进制")?;
    if raw.len() != 32 {
        bail!("WF_KEK 长度错误：{} 字节（需要 32）", raw.len());
    }
    let mut k = [0u8; 32];
    k.copy_from_slice(&raw);
    Ok(k)
}

/// 读取或生成主密钥：`WF_KEK`（64 位 hex）优先，否则 `<data_dir>/kek.bin`。
pub fn load_or_create_kek(data_dir: &Path) -> Result<[u8; 32]> {
    if let Ok(s) = std::env::var("WF_KEK") {
        let raw = hex::decode(s.trim()).context("WF_KEK 需要 64 位十六进制")?;
        if raw.len() != 32 {
            bail!("WF_KEK 长度错误：{} 字节（需要 32）", raw.len());
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&raw);
        return Ok(k);
    }
    let path = data_dir.join("kek.bin");
    if path.exists() {
        let raw = std::fs::read(&path)?;
        if raw.len() != 32 {
            bail!("{} 内容不是 32 字节", path.display());
        }
        let mut k = [0u8; 32];
        k.copy_from_slice(&raw);
        return Ok(k);
    }
    let k = rand_bytes(32);
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(&path, &k)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    log_line(format!("已生成主密钥 {}", path.display()));
    let mut kk = [0u8; 32];
    kk.copy_from_slice(&k);
    Ok(kk)
}

/// 生成/读取下载直链签名密钥（与 KEK 分开存放）。
pub fn load_or_create_url_secret(data_dir: &Path) -> Result<Vec<u8>> {
    if let Ok(s) = std::env::var("WF_URL_SECRET") {
        return Ok(s.into_bytes());
    }
    let path = data_dir.join("url_secret.bin");
    if path.exists() {
        return Ok(std::fs::read(&path)?);
    }
    let s = rand_bytes(32);
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(&path, &s)?;
    Ok(s)
}

fn log_line(s: String) {
    println!("[wf-auth] {s}");
}

// ---------------------------------------------------------------- 文件加密

pub struct EncryptedFile {
    pub cipher: Vec<u8>,
    pub file_nonce: [u8; 12],
    /// 由 KEK 包裹的 CEK（48 字节 = 32 + 16 tag）
    pub cek_wrapped: Vec<u8>,
    pub cek_wrap_nonce: [u8; 12],
}

fn cipher_for(key: &[u8]) -> Result<Aes256Gcm> {
    Aes256Gcm::new_from_slice(key).map_err(|e| anyhow!("AES 密钥错误: {e}"))
}

/// 上传时：明文 → 密文（服务端只留密文；CEK 由 KEK 包裹后入库）。
pub fn encrypt_file(kek: &[u8; 32], plaintext: &[u8]) -> Result<EncryptedFile> {
    let mut cek = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut cek);
    let mut file_nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut file_nonce);

    // 文件本体用 CEK 加密（输出含 16B tag）
    let cipher = cipher_for(&cek)?
        .encrypt(
            Nonce::from_slice(&file_nonce),
            Payload { msg: plaintext, aad: b"wf-file-v1" },
        )
        .map_err(|_| anyhow!("文件加密失败"))?;

    // 用 KEK 包裹 CEK
    let mut cek_wrap_nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut cek_wrap_nonce);
    let cek_wrapped = cipher_for(kek)?
        .encrypt(
            Nonce::from_slice(&cek_wrap_nonce),
            Payload { msg: &cek, aad: b"wf-cek-v1" },
        )
        .map_err(|_| anyhow!("CEK 包裹失败"))?;

    Ok(EncryptedFile {
        cipher,
        file_nonce,
        cek_wrapped,
        cek_wrap_nonce,
    })
}

/// 下载时：KEK 解出 CEK（每次下载都重新解，不在内存常驻）。
pub fn unwrap_cek(kek: &[u8; 32], cek_wrapped: &[u8], nonce: &[u8]) -> Result<[u8; 32]> {
    let cek = cipher_for(kek)?
        .decrypt(
            Nonce::from_slice(nonce),
            Payload { msg: cek_wrapped, aad: b"wf-cek-v1" },
        )
        .map_err(|_| anyhow!("CEK 解包失败（KEK 不匹配或数据损坏）"))?;
    if cek.len() != 32 {
        bail!("CEK 长度错误");
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&cek);
    Ok(out)
}

/// 服务端自校验用：解出明文（仅自测/排障，正常下载路径不调用）。
pub fn decrypt_file(cek: &[u8; 32], nonce: &[u8], cipher: &[u8]) -> Result<Vec<u8>> {
    cipher_for(cek)?
        .decrypt(
            Nonce::from_slice(nonce),
            Payload { msg: cipher, aad: b"wf-file-v1" },
        )
        .map_err(|_| anyhow!("文件解密失败"))
}

// ---------------------------------------------------------------- 设备包裹（ECIES）

pub struct KeyWrap {
    pub epk: [u8; 32],
    pub salt: [u8; 16],
    pub wrap_nonce: [u8; 12],
    pub cipher: Vec<u8>,
}

/// 把 CEK 包裹给某台设备：临时 X25519 + HKDF-SHA256 + AES-256-GCM（前向保密）。
pub fn wrap_cek_for_device(cek: &[u8; 32], device_pub: &[u8; 32]) -> Result<KeyWrap> {
    let mut eph_bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut eph_bytes);
    let eph = StaticSecret::from(eph_bytes);
    let epk = PublicKey::from(&eph).to_bytes();

    let shared = eph.diffie_hellman(&PublicKey::from(*device_pub));
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let hk = Hkdf::<Sha256>::new(Some(&salt), shared.as_bytes());
    let mut kek_ecdh = [0u8; 32];
    hk.expand(WRAP_INFO, &mut kek_ecdh)
        .map_err(|_| anyhow!("HKDF 派生失败"))?;

    let mut wrap_nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut wrap_nonce);
    let cipher = cipher_for(&kek_ecdh)?
        .encrypt(
            Nonce::from_slice(&wrap_nonce),
            Payload { msg: cek, aad: b"" },
        )
        .map_err(|_| anyhow!("CEK 包裹失败"))?;

    Ok(KeyWrap {
        epk,
        salt,
        wrap_nonce,
        cipher,
    })
}

// ---------------------------------------------------------------- 直链签名

pub fn sign_download(secret: &[u8], file_id: &str, exp: i64, uid: &str) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("hmac key");
    mac.update(format!("{file_id}|{exp}|{uid}").as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

pub fn verify_download(secret: &[u8], file_id: &str, exp: i64, uid: &str, sig: &str) -> bool {
    let expect = sign_download(secret, file_id, exp, uid);
    // 常数时间比较
    expect.len() == sig.len()
        && expect
            .bytes()
            .zip(sig.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

// ---------------------------------------------------------------- 兑换码

/// Crockford 风格字母表：去掉容易混淆的 0/O/1/I/L。
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

/// 生成一枚兑换码（形如 `AWOS-7K3M-9PQR-2XTV`）。
pub fn generate_code(prefix: &str, groups: usize, group_len: usize) -> String {
    let mut rng = rand::thread_rng();
    let mut parts = Vec::with_capacity(groups);
    for _ in 0..groups {
        let s: String = (0..group_len)
            .map(|_| CODE_ALPHABET[(rng.next_u32() as usize) % CODE_ALPHABET.len()] as char)
            .collect();
        parts.push(s);
    }
    let body = parts.join("-");
    if prefix.is_empty() {
        body
    } else {
        format!("{}-{}", prefix.trim_end_matches('-'), body)
    }
}

/// 归一化（大小写/空格/连字符不敏感）后取哈希入库。
pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub fn code_hash(code: &str) -> String {
    sha256_hex(normalize_code(code).as_bytes())
}

// ---------------------------------------------------------------- license token

pub fn new_token() -> String {
    b64url(&rand_bytes(32))
}

pub fn token_hash(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_file_and_wrap() {
        let kek = [7u8; 32];
        let plain = b"hello watchface".to_vec();
        let enc = encrypt_file(&kek, &plain).unwrap();
        let cek = unwrap_cek(&kek, &enc.cek_wrapped, &enc.cek_wrap_nonce).unwrap();
        let back = decrypt_file(&cek, &enc.file_nonce, &enc.cipher).unwrap();
        assert_eq!(back, plain);

        // 设备侧解包
        let mut dev_priv = [3u8; 32];
        dev_priv[0] = 9;
        let dev = StaticSecret::from(dev_priv);
        let dev_pub = PublicKey::from(&dev).to_bytes();
        let wrap = wrap_cek_for_device(&cek, &dev_pub).unwrap();

        let eph_pub = PublicKey::from(wrap.epk);
        let shared = dev.diffie_hellman(&eph_pub);
        let hk = Hkdf::<Sha256>::new(Some(&wrap.salt), shared.as_bytes());
        let mut kek_ecdh = [0u8; 32];
        hk.expand(WRAP_INFO, &mut kek_ecdh).unwrap();
        let cek2 = cipher_for(&kek_ecdh)
            .unwrap()
            .decrypt(
                Nonce::from_slice(&wrap.wrap_nonce),
                Payload { msg: &wrap.cipher, aad: b"" },
            )
            .unwrap();
        assert_eq!(cek2, cek.to_vec());
    }
}
