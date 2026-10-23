//! 窗口几何：多显示器边界夹取、DPI 换算与启动/缩放时的位置恢复。
//!
//! 这里全部是纯计算或轻量的窗口 API 调用，不碰笔记文件。

use crate::{settings::WindowGeometry, AppState};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, WebviewWindow};

fn top_right_position(window: &WebviewWindow) -> Option<(i32, i32)> {
    let monitor = window.primary_monitor().ok().flatten()?;
    let area = monitor.work_area();
    let window_size = window.inner_size().unwrap_or(PhysicalSize::new(480, 360));
    let margin = (24.0 * monitor.scale_factor()) as i32;
    Some((
        area.position.x + area.size.width as i32 - window_size.width as i32 - margin,
        area.position.y + margin,
    ))
}

/// 物理像素矩形，用于窗口与显示器工作区之间的夹取计算。
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn right(self) -> i32 {
        self.x.saturating_add(self.w.min(i32::MAX as u32) as i32)
    }

    fn bottom(self) -> i32 {
        self.y.saturating_add(self.h.min(i32::MAX as u32) as i32)
    }
}

fn overlap_area(a: Rect, b: Rect) -> i64 {
    let left = a.x.max(b.x) as i64;
    let top = a.y.max(b.y) as i64;
    let right = a.right().min(b.right()) as i64;
    let bottom = a.bottom().min(b.bottom()) as i64;
    if right <= left || bottom <= top {
        0
    } else {
        (right - left) * (bottom - top)
    }
}

fn monitor_rects(window: &WebviewWindow) -> Vec<Rect> {
    window
        .available_monitors()
        .map(|monitors| {
            monitors
                .iter()
                .map(|monitor| {
                    let area = monitor.work_area();
                    Rect {
                        x: area.position.x,
                        y: area.position.y,
                        w: area.size.width,
                        h: area.size.height,
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 把窗口位置夹回某块显示器的工作区：优先选择与保存位置重叠面积最大的屏幕，
/// 完全不相交时退到中心点最近的一块。
pub(crate) fn clamp_rect(rect: Rect, monitors: &[Rect]) -> (i32, i32) {
    let Some(first) = monitors.first() else {
        return (rect.x, rect.y);
    };

    let mut best = *first;
    let mut best_area = overlap_area(rect, best);
    for monitor in monitors.iter().skip(1) {
        let area = overlap_area(rect, *monitor);
        if area > best_area {
            best = *monitor;
            best_area = area;
        }
    }

    if best_area == 0 {
        let center_x = rect.x.saturating_add(rect.w as i32 / 2);
        let center_y = rect.y.saturating_add(rect.h as i32 / 2);
        best = *monitors
            .iter()
            .min_by_key(|monitor| {
                let dx = monitor.x.saturating_add(monitor.w as i32 / 2) - center_x;
                let dy = monitor.y.saturating_add(monitor.h as i32 / 2) - center_y;
                dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
            })
            .unwrap_or(first);
    }

    // 手改设置可能塞进超大尺寸，先夹到 i32 内再参与运算，避免溢出 panic
    let rect_w = rect.w.min(i32::MAX as u32) as i32;
    let rect_h = rect.h.min(i32::MAX as u32) as i32;
    let best_w = best.w.min(i32::MAX as u32) as i32;
    let best_h = best.h.min(i32::MAX as u32) as i32;

    // 至少保留左侧 120px、顶部 40px（标题栏）可见，保证窗口可以被拖回来
    let visible_x = 120.min(rect_w);
    let visible_y = 40.min(rect_h);
    let min_x = best.x.saturating_sub(rect_w.saturating_sub(visible_x));
    let max_x = best.x.saturating_add(best_w).saturating_sub(visible_x);
    let min_y = best.y;
    let max_y = best.y.saturating_add(best_h).saturating_sub(visible_y);

    let x = if min_x <= max_x {
        rect.x.clamp(min_x, max_x)
    } else {
        best.x
    };
    let y = if min_y <= max_y {
        rect.y.clamp(min_y, max_y)
    } else {
        best.y
    };

    (x, y)
}

/// 把保存的物理像素几何从旧 DPI 缩放换算到当前缩放，保证逻辑位置/尺寸不变。
/// 老配置没有缩放字段（None）或数值异常时原样返回。
pub(crate) fn scale_geometry(rect: Rect, saved_scale: Option<f64>, current_scale: f64) -> Rect {
    let factor = match saved_scale {
        Some(saved) if saved > 0.0 && current_scale > 0.0 => current_scale / saved,
        _ => return rect,
    };
    if (factor - 1.0).abs() < 1e-9 {
        return rect;
    }

    let point = |value: i32| -> i32 {
        (value as f64 * factor)
            .round()
            .clamp(i32::MIN as f64, i32::MAX as f64) as i32
    };
    let extent =
        |value: u32| -> u32 { (value as f64 * factor).round().clamp(0.0, u32::MAX as f64) as u32 };

    Rect {
        x: point(rect.x),
        y: point(rect.y),
        w: extent(rect.w),
        h: extent(rect.h),
    }
}

/// 尺寸限制在最小窗口尺寸与所在显示器工作区之间。
pub(crate) fn clamp_size(w: u32, h: u32, x: i32, y: i32, monitors: &[Rect]) -> (u32, u32) {
    const MIN_W: u32 = 380;
    const MIN_H: u32 = 240;

    let monitor = monitors
        .iter()
        .find(|monitor| {
            x >= monitor.x
                && x < monitor.x.saturating_add(monitor.w as i32)
                && y >= monitor.y
                && y < monitor.y.saturating_add(monitor.h as i32)
        })
        .or_else(|| monitors.first());

    match monitor {
        Some(monitor) => (
            w.clamp(MIN_W, monitor.w.max(MIN_W)),
            h.clamp(MIN_H, monitor.h.max(MIN_H)),
        ),
        None => (w.max(MIN_W), h.max(MIN_H)),
    }
}

fn clamp_position(window: &WebviewWindow, x: i32, y: i32) -> (i32, i32) {
    let monitors = monitor_rects(window);
    if monitors.is_empty() {
        return top_right_position(window).unwrap_or((x, y));
    }
    let size = window.inner_size().unwrap_or(PhysicalSize::new(480, 360));
    clamp_rect(
        Rect {
            x,
            y,
            w: size.width,
            h: size.height,
        },
        &monitors,
    )
}

pub fn apply_initial_geometry(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let saved = app
        .state::<AppState>()
        .settings
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .window
        .clone();

    match saved {
        Some(geometry) => {
            let monitors = monitor_rects(&window);
            // 物理像素会随 DPI 缩放变化，按保存时的缩放换算回同样的逻辑位置/尺寸
            let current_scale = window.scale_factor().unwrap_or(1.0);
            let scaled = scale_geometry(
                Rect {
                    x: geometry.x,
                    y: geometry.y,
                    w: geometry.w,
                    h: geometry.h,
                },
                geometry.scale,
                current_scale,
            );
            let (x, y) = clamp_rect(scaled, &monitors);
            let (w, h) = clamp_size(scaled.w, scaled.h, x, y, &monitors);
            if let Err(err) = window.set_position(PhysicalPosition::new(x, y)) {
                log::warn!("恢复窗口位置失败：{err}");
            }
            if let Err(err) = window.set_size(PhysicalSize::new(w, h)) {
                log::warn!("恢复窗口尺寸失败：{err}");
            }
        }
        None => {
            if let Some((x, y)) = top_right_position(&window) {
                if let Err(err) = window.set_position(PhysicalPosition::new(x, y)) {
                    log::warn!("设置窗口初始位置失败：{err}");
                }
            }
        }
    }
}

/// 系统缩放比例变化（切换显示器、修改缩放）后重新夹取窗口位置。
pub fn reclamp_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let Ok(position) = window.outer_position() else {
        return;
    };
    let (x, y) = clamp_position(&window, position.x, position.y);
    if (x, y) != (position.x, position.y) {
        if let Err(err) = window.set_position(PhysicalPosition::new(x, y)) {
            log::warn!("缩放变化后调整窗口位置失败：{err}");
        }
    }
}

/// 退出路径上抓一次窗口位置：hide 之外（托盘退出、异常结束）的移动也不会丢。
pub(crate) fn persist_geometry(app: &AppHandle, window: &WebviewWindow) {
    if let (Ok(pos), Ok(size)) = (window.outer_position(), window.inner_size()) {
        if let Ok(mut settings) = app.state::<AppState>().settings.lock() {
            settings.window = Some(WindowGeometry {
                x: pos.x,
                y: pos.y,
                w: size.width,
                h: size.height,
                scale: window.scale_factor().ok(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_rect_keeps_window_in_place_when_visible() {
        let monitors = [
            Rect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1040,
            },
            Rect {
                x: 1920,
                y: 0,
                w: 1920,
                h: 1040,
            },
        ];

        assert_eq!(
            clamp_rect(
                Rect {
                    x: 100,
                    y: 100,
                    w: 480,
                    h: 360
                },
                &monitors
            ),
            (100, 100)
        );
        assert_eq!(
            clamp_rect(
                Rect {
                    x: 2000,
                    y: 200,
                    w: 480,
                    h: 360
                },
                &monitors
            ),
            (2000, 200)
        );
    }

    #[test]
    fn clamp_rect_pulls_back_offscreen_window() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];

        let (x, y) = clamp_rect(
            Rect {
                x: 5000,
                y: 5000,
                w: 480,
                h: 360,
            },
            &monitors,
        );

        assert_eq!(x, 1920 - 120);
        assert_eq!(y, 1040 - 40);
    }

    #[test]
    fn clamp_rect_falls_back_to_nearest_monitor_after_unplug() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];

        let (x, y) = clamp_rect(
            Rect {
                x: 2500,
                y: 100,
                w: 480,
                h: 360,
            },
            &monitors,
        );

        assert!(x <= 1920 - 120);
        assert!(y >= 0);
    }

    #[test]
    fn scale_geometry_follows_dpi_changes() {
        let rect = Rect {
            x: 100,
            y: 50,
            w: 480,
            h: 360,
        };

        let scaled = scale_geometry(rect, Some(1.0), 1.5);
        assert_eq!(
            (scaled.x, scaled.y, scaled.w, scaled.h),
            (150, 75, 720, 540)
        );

        // 老配置没有 scale 字段：保持原样，不能凭空放大
        let untouched = scale_geometry(rect, None, 2.0);
        assert_eq!((untouched.x, untouched.y, untouched.w), (100, 50, 480));
    }

    #[test]
    fn clamp_rect_survives_absurd_saved_size() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];
        // 手改设置塞进 u32::MAX 时不能溢出 panic
        let (x, y) = clamp_rect(
            Rect {
                x: 100,
                y: 50,
                w: u32::MAX,
                h: i32::MAX as u32,
            },
            &monitors,
        );
        assert_eq!(x, 100);
        assert_eq!(y, 50);
    }

    #[test]
    fn clamp_rect_without_monitors_is_a_no_op() {
        let rect = Rect {
            x: 123,
            y: 456,
            w: 480,
            h: 360,
        };
        assert_eq!(clamp_rect(rect, &[]), (123, 456));
    }

    #[test]
    fn clamp_size_respects_minimum_and_monitor_bounds() {
        let monitors = [Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        }];

        assert_eq!(clamp_size(100, 100, 10, 10, &monitors), (380, 240));
        assert_eq!(clamp_size(4000, 3000, 10, 10, &monitors), (1920, 1040));
    }
}
