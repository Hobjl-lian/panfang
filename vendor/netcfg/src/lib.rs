//! 本地占位实现：上游 netcfg 为私有模块，account 仅用到这两个入口。
//! 提供与语义一致的默认 reqwest 客户端（带 cookie 存储）。

use reqwest::{Client, ClientBuilder};
use std::sync::Arc;
use std::time::Duration;

pub fn default_client_builder() -> ClientBuilder {
    Client::builder()
        .user_agent("Mozilla/5.0 (Linux; Android 13) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Mobile Safari/537.36")
        .cookie_provider(Arc::new(reqwest::cookie::Jar::default()))
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::default())
}

pub fn default_client() -> Client {
    default_client_builder().build().expect("failed to build http client")
}
