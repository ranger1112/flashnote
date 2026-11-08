//! 本地应用锁：Argon2id 密码验证器与界面访问控制。
//!
//! 这是「界面锁」，不是笔记加密：笔记、草稿、备份、Git 历史与同步远端仍是明文。
//! 密码验证器只存 Windows 凭据管理器，不写 settings.json，也不返回给前端。

pub(crate) mod runtime;
pub(crate) mod secrets;

use argon2::password_hash::{
    rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
};
use argon2::{Algorithm, Argon2, Params, Version};

pub(crate) const LOCKED_ERROR: &str = "应用已锁定，请先解锁";
pub(crate) const MIN_PASSWORD_CHARS: usize = 8;
pub(crate) const MAX_PASSWORD_CHARS: usize = 128;

fn hasher() -> Result<Argon2<'static>, String> {
    let params =
        Params::new(19_456, 2, 1, None).map_err(|error| format!("应用锁参数无效：{error}"))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

pub(crate) fn validate_new_password(password: &str) -> Result<(), String> {
    let count = password.chars().count();
    if count < MIN_PASSWORD_CHARS {
        return Err(format!("密码至少需要 {MIN_PASSWORD_CHARS} 个字符"));
    }
    if count > MAX_PASSWORD_CHARS {
        return Err(format!("密码不能超过 {MAX_PASSWORD_CHARS} 个字符"));
    }
    if password.chars().all(char::is_whitespace) {
        return Err("密码不能全是空白字符".to_string());
    }
    Ok(())
}

pub(crate) fn hash_password(password: &str) -> Result<String, String> {
    validate_new_password(password)?;
    let salt = SaltString::generate(&mut OsRng);
    hasher()?
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| format!("生成应用锁密码失败：{error}"))
}

/// 校验密码。格式错误的 PHC 返回 Err，而不是当作「密码错误」。
pub(crate) fn verify_password(password: &str, phc: &str) -> Result<bool, String> {
    let parsed = PasswordHash::new(phc).map_err(|error| format!("应用锁凭据损坏：{error}"))?;
    Ok(hasher()?
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_round_trips_and_wrong_password_fails() {
        let phc = hash_password("correct horse battery").unwrap();
        assert!(phc.starts_with("$argon2id$"), "{phc}");
        assert!(verify_password("correct horse battery", &phc).unwrap());
        assert!(!verify_password("wrong password", &phc).unwrap());
    }

    #[test]
    fn malformed_verifier_is_an_error_not_a_wrong_password() {
        assert!(verify_password("anything", "not-a-phc").is_err());
    }

    #[test]
    fn password_length_is_bounded() {
        assert!(validate_new_password("1234567").is_err());
        assert!(validate_new_password("12345678").is_ok());
        assert!(validate_new_password(&"a".repeat(MAX_PASSWORD_CHARS)).is_ok());
        assert!(validate_new_password(&"a".repeat(MAX_PASSWORD_CHARS + 1)).is_err());
        assert!(validate_new_password("        ").is_err());
    }

    #[test]
    fn unicode_passwords_are_supported() {
        let phc = hash_password("闪记密码-安全锁").unwrap();
        assert!(verify_password("闪记密码-安全锁", &phc).unwrap());
        assert!(!verify_password("闪记密码-错误锁", &phc).unwrap());
    }
}
