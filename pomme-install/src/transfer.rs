use std::future::Future;

use crate::{Fetch, Hash, Progress};

pub struct Verified<'a> {
    pub url: &'a str,
    pub expected: &'a str,
    pub hash: Hash,
}

pub trait Transfer {
    fn get_bytes(&self, url: &str) -> impl Future<Output = Result<Vec<u8>, String>>;

    fn fetch_verified(
        &self,
        request: &Verified<'_>,
        on_bytes: &(dyn Fn(u64) + Send + Sync),
    ) -> impl Future<Output = Result<Vec<u8>, String>>;

    fn download_all(
        &self,
        files: Vec<Fetch>,
        on_progress: &(dyn Fn(Progress) + Send + Sync),
    ) -> impl Future<Output = Result<(), String>>;

    fn stores_externally(&self) -> bool {
        false
    }
}

pub struct HttpTransfer {
    http: reqwest::Client,
}

impl HttpTransfer {
    pub fn new(http: reqwest::Client) -> Self {
        Self { http }
    }
}

impl Default for HttpTransfer {
    fn default() -> Self {
        Self::new(reqwest::Client::new())
    }
}

impl Transfer for HttpTransfer {
    async fn get_bytes(&self, url: &str) -> Result<Vec<u8>, String> {
        crate::with_retries(url, || crate::fetch(&self.http, url)).await
    }

    async fn fetch_verified(
        &self,
        request: &Verified<'_>,
        on_bytes: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<Vec<u8>, String> {
        crate::fetch_verified_with_progress(
            &self.http,
            request.url,
            request.expected,
            request.hash,
            on_bytes,
        )
        .await
    }

    async fn download_all(
        &self,
        files: Vec<Fetch>,
        on_progress: &(dyn Fn(Progress) + Send + Sync),
    ) -> Result<(), String> {
        crate::download_all(&self.http, files, on_progress).await
    }
}
