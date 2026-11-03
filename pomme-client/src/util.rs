
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct JavaRandom {
    seed: u64,
}

impl JavaRandom {
    const MULTIPLIER: u64 = 0x5DEECE66D;
    const INCREMENT: u64 = 0xB;
    const MASK: u64 = (1 << 48) - 1;

    pub fn new(seed: i64) -> Self {
        let mut rng = Self { seed: 0 };
        rng.set_seed(seed);
        rng
    }

    pub fn from_time() -> Self {
        let nanos = crate::clock::SystemTime::now()
            .duration_since(crate::clock::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as i64 + d.as_secs() as i64)
            .unwrap_or(0);
        Self::new(nanos)
    }

    pub fn set_seed(&mut self, seed: i64) {
        self.seed = (seed as u64 ^ Self::MULTIPLIER) & Self::MASK;
    }

    fn next(&mut self, bits: u32) -> i32 {
        self.seed = self
            .seed
            .wrapping_mul(Self::MULTIPLIER)
            .wrapping_add(Self::INCREMENT)
            & Self::MASK;
        (self.seed >> (48 - bits)) as i32
    }

    pub fn next_float(&mut self) -> f32 {
        self.next(24) as f32 / (1u32 << 24) as f32
    }

    pub fn next_int(&mut self, bound: i32) -> i32 {
        assert!(bound > 0);
        if bound & (bound - 1) == 0 {
            return ((bound as i64).wrapping_mul(self.next(31) as i64) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let val = bits % bound;
            if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                return val;
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    pomme_vfs::write(path, bytes)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut tmp = target.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let result = fs::File::create(&tmp)
        .and_then(|mut file| file.write_all(bytes))
        .and_then(|()| fs::rename(&tmp, &target));
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(target_os = "android")]
pub static ANDROID_APP: std::sync::RwLock<Option<crate::AndroidApp>> = std::sync::RwLock::new(None);

pub fn open_url(url: &str) {
    #[cfg(target_os = "android")]
    let opened = open_url_android(url).map_err(|e| e.to_string());
    #[cfg(target_os = "ios")]
    let opened = open_url_ios(url);
    #[cfg(all(not(mobile), feature = "game"))]
    let opened = open::that(url).map_err(|e| e.to_string());
    #[cfg(all(not(mobile), not(feature = "game")))]
    let opened: Result<(), String> = Err("opening links needs the `game` feature".to_owned());
    if let Err(e) = opened {
        tracing::warn!("Could not open {url:?}: {e}");
    }
}

#[cfg(target_os = "ios")]
pub fn exclude_from_backup(dir: &std::path::Path) {
    use objc2_foundation::{NSNumber, NSString, NSURL, NSURLIsExcludedFromBackupKey};

    let _ = std::fs::create_dir_all(dir);
    let url = NSURL::fileURLWithPath(&NSString::from_str(&dir.to_string_lossy()));
    let excluded = unsafe {
        url.setResourceValue_forKey_error(
            Some(&NSNumber::new_bool(true)),
            NSURLIsExcludedFromBackupKey,
        )
    };
    if let Err(e) = excluded {
        tracing::warn!("Couldn't keep {} out of backups: {e}", dir.display());
    }
}

#[cfg(target_os = "ios")]
fn open_url_ios(url: &str) -> Result<(), String> {
    use objc2_foundation::{MainThreadMarker, NSDictionary, NSString, NSURL};
    use objc2_ui_kit::UIApplication;

    let mtm = MainThreadMarker::new().ok_or("not on the main thread")?;
    let url = NSURL::URLWithString(&NSString::from_str(url)).ok_or("not a URL")?;
    let app = UIApplication::sharedApplication(mtm);
    unsafe { app.openURL_options_completionHandler(&url, &NSDictionary::new(), None) };
    Ok(())
}

#[cfg(target_os = "android")]
fn open_url_android(url: &str) -> jni::errors::Result<()> {
    use jni::objects::{JObject, JValue};
    use jni::{jni_sig, jni_str};

    let Some(app) = ANDROID_APP.read().ok().and_then(|app| app.clone()) else {
        return Ok(());
    };
    let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr().cast()) };
    vm.attach_current_thread(|env| {
        let activity = unsafe { JObject::from_raw(env, app.activity_as_ptr().cast()) };
        let url = env.new_string(url)?;
        let uri = env
            .call_static_method(
                jni_str!("android/net/Uri"),
                jni_str!("parse"),
                jni_sig!("(Ljava/lang/String;)Landroid/net/Uri;"),
                &[JValue::Object(&url)],
            )?
            .l()?;
        let action = env.new_string("android.intent.action.VIEW")?;
        let intent = env.new_object(
            jni_str!("android/content/Intent"),
            jni_sig!("(Ljava/lang/String;Landroid/net/Uri;)V"),
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        let extras = env.new_object(jni_str!("android/os/Bundle"), jni_sig!("()V"), &[])?;
        let session = env.new_string("android.support.customtabs.extra.SESSION")?;
        env.call_method(
            &extras,
            jni_str!("putBinder"),
            jni_sig!("(Ljava/lang/String;Landroid/os/IBinder;)V"),
            &[JValue::Object(&session), JValue::Object(&JObject::null())],
        )?;
        env.call_method(
            &intent,
            jni_str!("putExtras"),
            jni_sig!("(Landroid/os/Bundle;)Landroid/content/Intent;"),
            &[JValue::Object(&extras)],
        )?;
        env.call_method(
            &activity,
            jni_str!("startActivity"),
            jni_sig!("(Landroid/content/Intent;)V"),
            &[JValue::Object(&intent)],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::test_temp_dir;

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = pomme_vfs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_temp() {
        let dir = test_temp_dir("write_atomic");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("options.json");
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(entries(&dir), ["options.json"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_atomic_failure_removes_temp() {
        let dir = test_temp_dir("write_atomic_failure");
        let target = dir.join("options.json");
        fs::create_dir_all(&target).unwrap();
        assert!(write_atomic(&target, b"x").is_err());
        assert_eq!(entries(&dir), ["options.json"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
