//! The browser's Cache storage as the memory tree's disk: files written into
//! the tree are kept there by [`persist`], and [`restore`] reads them back on
//! the next visit.

use std::path::{Path, PathBuf};

use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Cache, CacheStorage, Request, Response};

use crate::memory::global::with;

/// Cache entries are keyed by URLs under this origin, which nothing serves.
const KEY_PREFIX: &str = "https://pomme-vfs.invalid/";

fn js_error(error: JsValue) -> String {
    error.as_string().unwrap_or_else(|| format!("{error:?}"))
}

async fn open(namespace: &str) -> Result<Cache, String> {
    let storage: CacheStorage = Reflect::get(&js_sys::global(), &JsValue::from_str("caches"))
        .map_err(js_error)?
        .dyn_into()
        .map_err(|_| "the browser has no Cache storage".to_owned())?;
    let cache = JsFuture::from(storage.open(&format!("pomme-vfs:{namespace}")))
        .await
        .map_err(js_error)?;
    cache
        .dyn_into()
        .map_err(|_| "the Cache storage opened something else".to_owned())
}

fn key(path: &Path) -> String {
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

/// Stores the file at `path` of the memory tree in the cache `namespace`.
pub async fn persist(namespace: &str, path: &Path) -> Result<(), String> {
    let mut bytes = crate::read(path).map_err(|e| e.to_string())?;
    let response = Response::new_with_opt_u8_array(Some(&mut bytes)).map_err(js_error)?;
    let cache = open(namespace).await?;
    JsFuture::from(cache.put_with_str(&key(path), &response))
        .await
        .map_err(js_error)?;
    Ok(())
}

/// Stores every file of the memory tree under `dir`.
pub async fn persist_dir(namespace: &str, dir: &Path) -> Result<(), String> {
    let paths: Vec<PathBuf> = with(|fs| {
        fs.files()
            .map(|(path, _)| path.to_path_buf())
            .filter(|path| path.starts_with(dir))
            .collect()
    });
    for path in paths {
        persist(namespace, &path).await?;
    }
    Ok(())
}

/// Reads every file of the cache `namespace` into the memory tree, returning
/// how many there were.
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
