//! 应用锁的运行时状态：解锁截止时间、失败退避与对外状态。
//!
//! 时间全部由调用方传入，测试可以精确控制单调时钟与墙上时钟。

use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use super::secrets;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Deadline {
    /// 墙上时钟：处理休眠/恢复后单调时钟继续走、但实际时间已过的情况。
    pub wall: SystemTime,
    /// 单调时钟：处理用户把系统时间往回调。
    pub mono: Instant,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LockStatus {
    pub configured: bool,
    pub locked: bool,
    pub remaining_seconds: Option<u64>,
    pub retry_after_seconds: Option<u64>,
    pub unlock_minutes: u32,
    pub theme: String,
    pub warning: Option<String>,
}

#[derive(Default)]
pub(crate) struct LockRuntime {
    verifier: Option<String>,
    unlocked_until: Option<Deadline>,
    failed_attempts: u32,
    blocked_until: Option<Instant>,
    /// 凭据读取失败时给设置页的提示；此时按未启用处理，避免把应用锁死。
    warning: Option<String>,
}

impl LockRuntime {
    pub(crate) fn from_store() -> Self {
        match secrets::load() {
            Ok(Some(verifier)) => Self {
                verifier: Some(verifier),
                ..Self::default()
            },
            Ok(None) => Self::default(),
            Err(error) => {
                log::warn!("读取应用锁凭据失败：{error}");
                Self {
                    warning: Some("无法读取应用锁凭据，本次启动未启用应用锁".to_string()),
                    ..Self::default()
                }
            }
        }
    }

    pub(crate) fn warning(&self) -> Option<String> {
        self.warning.clone()
    }

    pub(crate) fn configured(&self) -> bool {
        self.verifier.is_some()
    }

    pub(crate) fn verifier(&self) -> Option<&str> {
        self.verifier.as_deref()
    }

    pub(crate) fn set_verifier(&mut self, verifier: Option<String>) {
        self.verifier = verifier;
        self.warning = None;
    }

    /// 过期时顺手清掉截止时间，避免每次检查都重复判断。
    pub(crate) fn is_locked(&mut self, mono: Instant, wall: SystemTime) -> bool {
        if !self.configured() {
            return false;
        }
        let Some(deadline) = self.unlocked_until else {
            return true;
        };
        if mono >= deadline.mono || wall >= deadline.wall {
            self.unlocked_until = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn remaining_seconds(&mut self, mono: Instant, wall: SystemTime) -> Option<u64> {
        if !self.configured() || self.is_locked(mono, wall) {
            return None;
        }
        let deadline = self.unlocked_until?;
        let mono_left = deadline.mono.saturating_duration_since(mono);
        let wall_left = deadline.wall.duration_since(wall).unwrap_or(Duration::ZERO);
        let left = mono_left.min(wall_left);
        let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
        Some(seconds.max(1))
    }

    pub(crate) fn retry_after_seconds(&self, mono: Instant) -> Option<u64> {
        let blocked = self.blocked_until?;
        if blocked <= mono {
            return None;
        }
        let left = blocked.duration_since(mono);
        Some((left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1))
    }

    pub(crate) fn precheck_unlock(&self, mono: Instant) -> Result<(), String> {
        match self.retry_after_seconds(mono) {
            Some(seconds) => Err(format!("尝试次数过多，请 {seconds} 秒后重试")),
            None => Ok(()),
        }
    }

    /// 记录一次密码错误，返回需要等待的秒数（0 表示无需等待）。
    pub(crate) fn record_failure(&mut self, mono: Instant) -> u64 {
        self.failed_attempts = self.failed_attempts.saturating_add(1);
        if self.failed_attempts <= 2 {
            return 0;
        }
        let exponent = (self.failed_attempts - 3).min(10);
        let seconds = 5u64.saturating_mul(1u64 << exponent).min(300);
        self.blocked_until = Some(mono + Duration::from_secs(seconds));
        seconds
    }

    pub(crate) fn record_success(&mut self, minutes: u32, mono: Instant, wall: SystemTime) {
        let duration = Duration::from_secs(u64::from(minutes.max(1)) * 60);
        self.unlocked_until = Some(Deadline {
            wall: wall.checked_add(duration).unwrap_or(wall),
            mono: mono + duration,
        });
        self.failed_attempts = 0;
        self.blocked_until = None;
    }

    pub(crate) fn force_lock(&mut self) {
        self.unlocked_until = None;
        // 手动上锁不重置失败计数：退避仍然有效。
    }

    pub(crate) fn reset_attempts(&mut self) {
        self.failed_attempts = 0;
        self.blocked_until = None;
    }

    pub(crate) fn status(
        &mut self,
        unlock_minutes: u32,
        theme: String,
        mono: Instant,
        wall: SystemTime,
    ) -> LockStatus {
        let locked = self.is_locked(mono, wall);
        LockStatus {
            configured: self.configured(),
            locked,
            remaining_seconds: self.remaining_seconds(mono, wall),
            retry_after_seconds: self.retry_after_seconds(mono),
            unlock_minutes,
            theme,
            warning: self.warning.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> LockRuntime {
        LockRuntime {
            verifier: Some("$argon2id$fake".to_string()),
            ..LockRuntime::default()
        }
    }

    #[test]
    fn unconfigured_is_never_locked() {
        let mut runtime = LockRuntime::default();
        let mono = Instant::now();
        let wall = SystemTime::now();
        assert!(!runtime.is_locked(mono, wall));
        assert_eq!(runtime.remaining_seconds(mono, wall), None);
    }

    #[test]
    fn configured_requires_unlock_and_expires() {
        let mut runtime = configured();
        let mono = Instant::now();
        let wall = SystemTime::now();
        assert!(runtime.is_locked(mono, wall));

        runtime.record_success(30, mono, wall);
        assert!(!runtime.is_locked(mono + Duration::from_secs(29 * 60), wall));
        assert!(!runtime.is_locked(mono + Duration::from_secs(29 * 60 + 59), wall));
        assert!(runtime.is_locked(mono + Duration::from_secs(30 * 60), wall));
    }

    #[test]
    fn wall_clock_jump_forward_locks_even_if_monotonic_is_short() {
        let mut runtime = configured();
        let mono = Instant::now();
        let wall = SystemTime::now();
        runtime.record_success(24 * 60, mono, wall);
        assert!(runtime.is_locked(mono, wall + Duration::from_secs(24 * 60 * 60)));
    }

    #[test]
    fn wall_clock_rollback_does_not_extend_validity() {
        let mut runtime = configured();
        let mono = Instant::now();
        let wall = SystemTime::now();
        runtime.record_success(30, mono, wall);
        assert!(runtime.is_locked(mono + Duration::from_secs(30 * 60), wall));
    }

    #[test]
    fn remaining_seconds_rounds_up_and_is_never_zero() {
        let mut runtime = configured();
        let mono = Instant::now();
        let wall = SystemTime::now();
        runtime.record_success(30, mono, wall);
        let elapsed = Duration::from_secs(28 * 60 + 59);
        assert_eq!(
            runtime.remaining_seconds(mono + elapsed, wall + elapsed),
            Some(61)
        );
    }

    #[test]
    fn failures_back_off_then_success_resets() {
        let mut runtime = configured();
        let mono = Instant::now();
        assert_eq!(runtime.record_failure(mono), 0);
        assert_eq!(runtime.record_failure(mono), 0);
        assert_eq!(runtime.record_failure(mono), 5);
        assert_eq!(runtime.record_failure(mono), 10);
        assert_eq!(runtime.retry_after_seconds(mono), Some(10));
        assert!(runtime.precheck_unlock(mono).is_err());

        runtime.record_success(30, mono, SystemTime::now());
        assert_eq!(runtime.retry_after_seconds(mono), None);
        assert!(runtime.precheck_unlock(mono).is_ok());
    }

    #[test]
    fn backoff_is_capped_at_five_minutes() {
        let mut runtime = configured();
        let mono = Instant::now();
        let mut last = 0;
        for _ in 0..20 {
            last = runtime.record_failure(mono);
        }
        assert_eq!(last, 300);
    }

    #[test]
    fn force_lock_keeps_failure_counter() {
        let mut runtime = configured();
        let mono = Instant::now();
        runtime.record_failure(mono);
        runtime.record_success(30, mono, SystemTime::now());
        runtime.force_lock();
        assert!(runtime.is_locked(mono, SystemTime::now()));
        assert_eq!(runtime.remaining_seconds(mono, SystemTime::now()), None);
    }

    #[test]
    fn status_serializes_camel_case() {
        let mut runtime = configured();
        let mono = Instant::now();
        let wall = SystemTime::now();
        runtime.record_success(30, mono, wall);
        let status = runtime.status(120, "dark".to_string(), mono, wall);
        let json = serde_json::to_value(status).unwrap();
        assert_eq!(json["unlockMinutes"], 120);
        assert_eq!(json["theme"], "dark");
        assert!(json.get("remainingSeconds").is_some());
        assert!(json.get("retryAfterSeconds").is_some());
    }
}
