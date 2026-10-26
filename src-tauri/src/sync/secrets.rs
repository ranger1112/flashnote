//! 同步密码：只存 Windows 凭据管理器，绝不写入设置文件或日志。
//!
//! 凭据管理器被组策略禁用时返回明确错误，调用方据此提示用户，并在本次
//! 会话内改用内存里的密码；任何情况下都不回退到明文落盘。

use super::state::sha256_base64;

const SERVICE: &str = "flashnote-sync";

/// 凭据账号名：规范化 URL 与用户名一起哈希，换服务器或换账号都不会串到旧密码。
///
/// 大小写、末尾斜杠与 query 都不改变远端身份，统一走 [`super::remote_identity`]，
/// 避免「补一个 /」或把域名改成小写就找不到密码。
pub(crate) fn account(url: &str, username: &str) -> String {
    sha256_base64(super::remote_identity(url, username).as_bytes())
}

/// 0.5.0 的账号名算法：原始 URL 只去掉末尾斜杠。升级后先按新名字查，查不到
/// 再回退到旧名字，命中后迁移，避免老用户突然「没有保存密码」。
fn legacy_account(url: &str, username: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    sha256_base64(format!("{}\n{}", url, username.trim()).as_bytes())
}

/// 保存密码；覆盖已有凭据。
pub(crate) fn save(url: &str, username: &str, password: &str) -> Result<(), String> {
    imp::save(url, username, password)
}

/// 读取密码；没有保存过时返回 `Ok(None)`。
pub(crate) fn load(url: &str, username: &str) -> Result<Option<String>, String> {
    imp::load(url, username)
}

/// 删除密码；本来就没有也算成功。
pub(crate) fn clear(url: &str, username: &str) -> Result<(), String> {
    imp::clear(url, username)
}

#[cfg(windows)]
mod imp {
    use super::*;

    fn entry(url: &str, username: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, &account(url, username))
            .map_err(|error| format!("系统凭据存储不可用：{error}"))
    }

    pub(super) fn save(url: &str, username: &str, password: &str) -> Result<(), String> {
        entry(url, username)?
            .set_password(password)
            .map_err(|error| format!("保存密码失败：{error}"))
    }

    pub(super) fn load(url: &str, username: &str) -> Result<Option<String>, String> {
        let current = entry(url, username)?;
        match current.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => {
                let legacy_id = legacy_account(url, username);
                if legacy_id == account(url, username) {
                    return Ok(None);
                }
                let legacy = keyring::Entry::new(SERVICE, &legacy_id)
                    .map_err(|error| format!("系统凭据存储不可用：{error}"))?;
                match legacy.get_password() {
                    Ok(password) => {
                        // 顺手迁移到新账号名，之后就不用再回退。
                        let _ = current.set_password(&password);
                        let _ = legacy.delete_credential();
                        Ok(Some(password))
                    }
                    Err(keyring::Error::NoEntry) => Ok(None),
                    Err(error) => Err(format!("读取密码失败：{error}")),
                }
            }
            Err(error) => Err(format!("读取密码失败：{error}")),
        }
    }

    pub(super) fn clear(url: &str, username: &str) -> Result<(), String> {
        match entry(url, username)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(format!("删除密码失败：{error}")),
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;

    const UNAVAILABLE: &str = "当前系统没有可用的凭据管理器，同步密码只能在本次会话内使用";

    pub(super) fn save(_url: &str, _username: &str, _password: &str) -> Result<(), String> {
        Err(UNAVAILABLE.to_string())
    }

    pub(super) fn load(_url: &str, _username: &str) -> Result<Option<String>, String> {
        Err(UNAVAILABLE.to_string())
    }

    pub(super) fn clear(_url: &str, _username: &str) -> Result<(), String> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_is_stable_and_sensitive_to_both_inputs() {
        let a = account("https://dav.example.com/", "alice");
        assert_eq!(a, account(" https://dav.example.com/ ", " alice "));
        assert_eq!(
            account("https://dav.example.com/dav", "alice"),
            account("https://dav.example.com/dav/", "alice"),
            "末尾斜杠不应改变凭据身份"
        );
        assert_eq!(
            account("https://DAV.example.com/dav", "alice"),
            account("https://dav.example.com/dav", "alice"),
            "域名大小写不改变凭据身份"
        );
        assert_eq!(
            account("https://dav.example.com/dav?token=1", "alice"),
            account("https://dav.example.com/dav", "alice"),
            "query 不改变凭据身份"
        );
        assert_ne!(a, account("https://dav.example.com/", "bob"));
        assert_ne!(a, account("https://other.example.com/", "alice"));
        assert_eq!(a.len(), 43, "SHA-256 的 Base64url 长度");
    }

    #[test]
    fn legacy_account_matches_the_0_5_0_algorithm() {
        assert_eq!(
            legacy_account(" https://dav.example.com/dav/ ", " alice "),
            sha256_base64(b"https://dav.example.com/dav\nalice")
        );
        // 大写域名在旧算法里会得到不同的账号，升级时确实需要回退查找。
        assert_ne!(
            legacy_account("https://DAV.example.com/dav", "alice"),
            account("https://DAV.example.com/dav", "alice")
        );
    }

    /// 手动验证 Windows 凭据管理器：`cargo test keyring_round_trip -- --ignored`
    #[cfg(windows)]
    #[test]
    #[ignore]
    fn keyring_round_trip() {
        let url = "https://flashnote.invalid/";
        let user = "keyring-test";
        save(url, user, "secret-1").unwrap();
        assert_eq!(load(url, user).unwrap().as_deref(), Some("secret-1"));
        save(url, user, "secret-2").unwrap();
        assert_eq!(load(url, user).unwrap().as_deref(), Some("secret-2"));
        clear(url, user).unwrap();
        assert!(load(url, user).unwrap().is_none());
    }
}
