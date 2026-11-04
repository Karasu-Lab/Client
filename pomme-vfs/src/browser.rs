use std::path::{Path, PathBuf};

use js_sys::{Array, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Cache, CacheStorage, Request, Response};

use crate::memory::global::with;

const KEY_PREFIX: &str = "https://pomme-vfs.invalid/";

fn js_error(error: JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}

pub fn cache_name(namespace: &str) -> String {
    format!("pomme-vfs:{namespace}")
}

async fn open(namespace: &str) -> Result<Cache, String> {
    let storage: CacheStorage = Reflect::get(&js_sys::global(), &JsValue::from_str("caches"))
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| "the browser has no Cache storage".to_owned())?;
    let cache = JsFuture::from(storage.open(&cache_name(namespace)))
        .await
        .map_err(js_error)?;
    cache
        .dyn_into()
        .map_err(|_| "the Cache storage opened something else".to_owned())
}

pub fn key(path: &Path) -> String {
    let mut url = String::from(KEY_PREFIX);
    for (i, part) in path.components().enumerate() {
        if i > 0 {
            url.push('/');
        }
        let part = part.as_os_str().to_string_lossy();
        url.push_str(&String::from(js_sys::encode_uri_component(&part)));
    }
    url
}

fn path_of(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix(KEY_PREFIX)?;
    let mut path = PathBuf::new();
    for part in rest.split('/') {
        path.push(String::from(js_sys::decode_uri_component(part).ok()?));
    }
    Some(path)
}

pub async fn persist(namespace: &str, path: &Path) -> Result<(), String> {
    let cache = open(namespace).await?;
    JsFuture::from(put(&cache, path)?).await.map_err(js_error)?;
    Ok(())
}

fn put(cache: &Cache, path: &Path) -> Result<Promise, String> {
    let bytes = crate::read(path).map_err(|e| e.to_string())?;
    let body = Uint8Array::new_with_length(bytes.len() as u32);
    body.copy_from(&bytes);
    let response = Response::new_with_opt_buffer_source(Some(&body)).map_err(js_error)?;
    Ok(cache.put_with_str(&key(path), &response))
}

const PERSIST_BATCH: usize = 64;

pub async fn persist_dir(namespace: &str, dir: &Path) -> Result<(), String> {
    persist_dir_with(namespace, dir, &[], &|_, _| {}).await
}

pub async fn persist_dir_with(
    namespace: &str,
    dir: &Path,
    last: &[PathBuf],
    on_progress: &dyn Fn(u32, u32),
) -> Result<(), String> {
    let paths: Vec<PathBuf> = with(|fs| {
        fs.files()
            .map(|(path, _)| path.to_path_buf())
            .filter(|path| path.starts_with(dir) && !last.contains(path))
            .collect()
    });
    let last: Vec<&PathBuf> = last.iter().filter(|path| path.starts_with(dir)).collect();
    let total = (paths.len() + last.len()) as u32;
    let cache = open(namespace).await?;
    let mut done = 0;
    on_progress(done, total);
    for batch in paths.chunks(PERSIST_BATCH) {
        let puts = Array::new();
        for path in batch {
            puts.push(&put(&cache, path)?.into());
        }
        JsFuture::from(Promise::all(&puts))
            .await
            .map_err(js_error)?;
        done += batch.len() as u32;
        on_progress(done, total);
    }
    for path in last {
        JsFuture::from(put(&cache, path)?).await.map_err(js_error)?;
        done += 1;
        on_progress(done, total);
    }
    Ok(())
}

pub async fn persist_files(
    namespace: &str,
    paths: &[PathBuf],
    on_progress: &dyn Fn(u32, u32),
) -> Result<(), String> {
    let total = paths.len() as u32;
    let cache = open(namespace).await?;
    on_progress(0, total);
    for (index, path) in paths.iter().enumerate() {
        JsFuture::from(put(&cache, path)?).await.map_err(js_error)?;
        on_progress(index as u32 + 1, total);
    }
    Ok(())
}

pub async fn restore_paths(namespace: &str, paths: &[PathBuf]) -> Result<usize, String> {
    let cache = open(namespace).await?;
    let mut restored = 0;
    for path in paths {
        let Some(response) = JsFuture::from(cache.match_with_str(&key(path)))
            .await
            .map_err(js_error)?
            .dyn_into::<Response>()
            .ok()
        else {
            continue;
        };
        let buffer = JsFuture::from(response.array_buffer().map_err(js_error)?)
            .await
            .map_err(js_error)?;
        let bytes = Uint8Array::new(&buffer).to_vec();
        with(|fs| fs.write(path, &bytes)).map_err(|e| e.to_string())?;
        restored += 1;
    }
    Ok(restored)
}

pub async fn restore(namespace: &str) -> Result<usize, String> {
    let cache = open(namespace).await?;
    let requests: Array = JsFuture::from(cache.keys())
        .await
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| "the cache listed something else".to_owned())?;
    let mut restored = 0;
    for request in requests.iter() {
        let request: Request = request
            .dyn_into()
            .map_err(|_| "the cache keyed something else".to_owned())?;
        let Some(path) = path_of(&request.url()) else {
            continue;
        };
        let Some(response) = JsFuture::from(cache.match_with_request(&request))
            .await
            .map_err(js_error)?
            .dyn_into::<Response>()
            .ok()
        else {
            continue;
        };
        let buffer = JsFuture::from(response.array_buffer().map_err(js_error)?)
            .await
            .map_err(js_error)?;
        let bytes = Uint8Array::new(&buffer).to_vec();
        with(|fs| fs.write(&path, &bytes)).map_err(|e| e.to_string())?;
        restored += 1;
    }
    Ok(restored)
}
