//! 应用锁验证器的凭据存储。
//!
//! 只存 Argon2 PHC 字符串，不存明文密码；条目属于当前 Windows 用户。
//! 凭据不存在 = 应用锁未启用。

const SERVICE: &str = "flashnote-app-lock";
const ACCOUNT: &str = "verifier-v1";

pub(crate) fn load() -> Result<Option<String>, String> {
    imp::load()
}

pub(crate) fn save(value: &str) -> Result<(), String> {
    imp::save(value)
}

pub(crate) fn clear() -> Result<(), String> {
    imp::clear()
}

#[cfg(windows)]
mod imp {
    use super::{ACCOUNT, SERVICE};
    use keyring::Entry;

    fn entry() -> Result<Entry, String> {
        Entry::new(SERVICE, ACCOUNT).map_err(|error| format!("系统凭据存储不可用：{error}"))
    }

    pub(super) fn load() -> Result<Option<String>, String> {
        match entry()?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(format!("读取应用锁凭据失败：{error}")),
        }
    }

    pub(super) fn save(value: &str) -> Result<(), String> {
        entry()?
            .set_password(value)
            .map_err(|error| format!("保存应用锁凭据失败：{error}"))
    }

    pub(super) fn clear() -> Result<(), String> {
        match entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(format!("删除应用锁凭据失败：{error}")),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub(super) fn load() -> Result<Option<String>, String> {
        Err("应用锁需要 Windows 凭据管理器".to_string())
    }

    pub(super) fn save(_: &str) -> Result<(), String> {
        Err("应用锁需要 Windows 凭据管理器".to_string())
    }

    pub(super) fn clear() -> Result<(), String> {
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use keyring::{Entry, Error};

    /// 手动验证 Windows 凭据管理器：
    /// `cargo test app_lock_keyring_round_trip -- --ignored`
    #[test]
    #[ignore]
    fn app_lock_keyring_round_trip() {
        let entry = Entry::new("flashnote-app-lock-test", "verifier-v1").unwrap();
        let _ = entry.delete_credential();
        entry.set_password("$argon2id$test").unwrap();
        assert_eq!(entry.get_password().unwrap(), "$argon2id$test");
        entry.set_password("$argon2id$test-2").unwrap();
        assert_eq!(entry.get_password().unwrap(), "$argon2id$test-2");
        entry.delete_credential().unwrap();
        assert!(matches!(entry.get_password(), Err(Error::NoEntry)));
    }
}
