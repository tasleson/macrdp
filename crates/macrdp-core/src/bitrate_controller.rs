use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Determine if an IP address belongs to a private/local network.
pub fn is_private_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Two-phase LAN detection result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkType {
    Lan,
    Wan,
}

/// Phase 1: IP-based initial detection.
pub fn detect_network_phase1(peer_addr: IpAddr) -> NetworkType {
    if is_private_ip(peer_addr) {
        NetworkType::Lan
    } else {
        NetworkType::Wan
    }
}

/// Phase 2: RTT-based correction. Call after GFX channel has ~5 RTT samples.
pub fn detect_network_phase2(phase1: NetworkType, rtt_ewma_ms: f64) -> NetworkType {
    const LAN_RTT_THRESHOLD_MS: f64 = 10.0;
    match phase1 {
        NetworkType::Lan if rtt_ewma_ms > LAN_RTT_THRESHOLD_MS => NetworkType::Wan,
        other => other,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FrameStats {
    pub encode_ms: f64,
    pub frame_bytes: u32,
    pub is_keyframe: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveDecision {
    pub bitrate_bps: u32,
    pub fps: u32,
}

const FPS_TIERS: [f32; 3] = [1.0, 0.5, 0.25];

pub struct BitrateController {
    initial_bitrate: u32,
    current_bitrate: u32,
    target_fps: u32,
    current_fps_tier: usize,
    eval_window: Vec<FrameStats>,
    last_eval_time: Instant,
    eval_interval: Duration,
    is_lan: bool,
}

impl BitrateController {
    pub fn new(initial_bitrate: u32, target_fps: u32, is_lan: bool) -> Self {
        Self {
            initial_bitrate,
            current_bitrate: initial_bitrate,
            target_fps,
            current_fps_tier: 0,
            eval_window: Vec::with_capacity(128),
            last_eval_time: Instant::now(),
            eval_interval: Duration::from_secs(1),
            is_lan,
        }
    }

    pub fn record_frame(&mut self, stats: FrameStats) {
        self.eval_window.push(stats);
    }

    pub fn current_bitrate(&self) -> u32 {
        self.current_bitrate
    }

    pub fn current_fps(&self) -> u32 {
        (self.target_fps as f32 * FPS_TIERS[self.current_fps_tier]) as u32
    }

    pub fn target_fps(&self) -> u32 {
        self.target_fps
    }

    pub fn is_lan(&self) -> bool {
        self.is_lan
    }

    pub fn update_network_type(&mut self, rtt_ewma_ms: f64) {
        if self.is_lan {
            let corrected = detect_network_phase2(NetworkType::Lan, rtt_ewma_ms);
            if corrected == NetworkType::Wan {
                self.is_lan = false;
                tracing::info!(
                    rtt_ewma_ms,
                    "LAN → WAN: RTT exceeds threshold, activating adaptive bitrate"
                );
            }
        }
    }

    pub fn on_idle_recovery(&mut self) {
        self.eval_window.clear();
        self.current_bitrate = self.initial_bitrate;
        self.current_fps_tier = 0;
        self.last_eval_time = Instant::now();
    }

    pub fn evaluate(&mut self) -> AdaptiveDecision {
        let current_fps = self.current_fps();
        if self.is_lan {
            self.eval_window.clear();
            self.last_eval_time = Instant::now();
            return AdaptiveDecision {
                bitrate_bps: self.current_bitrate,
                fps: current_fps,
            };
        }
        if self.eval_window.is_empty() {
            return AdaptiveDecision {
                bitrate_bps: self.current_bitrate,
                fps: current_fps,
            };
        }

        let frame_interval_ms = 1000.0 / current_fps as f64;
        let total = self.eval_window.len() as f64;
        let avg_encode_ms = self.eval_window.iter().map(|s| s.encode_ms).sum::<f64>() / total;
        let non_kf: Vec<_> = self.eval_window.iter().filter(|s| !s.is_keyframe).collect();
        let avg_frame_bytes = if non_kf.is_empty() {
            0.0
        } else {
            non_kf.iter().map(|s| s.frame_bytes as f64).sum::<f64>() / non_kf.len() as f64
        };

        let floor = (self.initial_bitrate as f64 * 0.3) as u32;
        let ceiling = self.initial_bitrate;
        let target_frame_bytes = self.current_bitrate as f64 / current_fps as f64 / 8.0;

        let mut new_bitrate = self.current_bitrate;
        let mut new_fps_tier = self.current_fps_tier;

        let encode_overloaded = avg_encode_ms > frame_interval_ms * 0.6;
        if encode_overloaded {
            new_bitrate = ((new_bitrate as f64) * 0.85) as u32;
        } else if avg_frame_bytes > 0.0
            && target_frame_bytes > 0.0
            && avg_frame_bytes > target_frame_bytes * 1.5
        {
            new_bitrate = ((new_bitrate as f64) * 0.90) as u32;
        }
        new_bitrate = new_bitrate.max(floor).min(ceiling);

        if new_bitrate <= floor
            && avg_encode_ms > frame_interval_ms * 0.8
            && new_fps_tier < FPS_TIERS.len() - 1
        {
            new_fps_tier += 1;
        }

        if avg_encode_ms < frame_interval_ms * 0.3 {
            if new_fps_tier > 0 {
                new_fps_tier -= 1;
            } else if new_bitrate < ceiling {
                new_bitrate = (((new_bitrate as f64) * 1.10) as u32).min(ceiling);
            }
        }

        self.current_bitrate = new_bitrate;
        self.current_fps_tier = new_fps_tier;
        self.eval_window.clear();
        self.last_eval_time = Instant::now();
        AdaptiveDecision {
            bitrate_bps: new_bitrate,
            fps: self.current_fps(),
        }
    }

    pub fn should_evaluate(&self) -> bool {
        self.last_eval_time.elapsed() >= self.eval_interval && !self.eval_window.is_empty()
    }

    #[cfg(test)]
    fn set_bitrate_to_floor_for_test(&mut self) {
        self.current_bitrate = (self.initial_bitrate as f64 * 0.3) as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn private_ipv4_ranges() {
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
    }

    #[test]
    fn public_ipv4() {
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
    }

    #[test]
    fn ipv6() {
        assert!(is_private_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_private_ip(IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(!is_private_ip(IpAddr::V6(Ipv6Addr::new(
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[test]
    fn phase1_detection() {
        assert_eq!(
            detect_network_phase1(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
            NetworkType::Lan
        );
        assert_eq!(
            detect_network_phase1(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))),
            NetworkType::Wan
        );
    }

    #[test]
    fn phase2_rtt_correction() {
        assert_eq!(
            detect_network_phase2(NetworkType::Lan, 50.0),
            NetworkType::Wan
        );
        assert_eq!(
            detect_network_phase2(NetworkType::Lan, 3.0),
            NetworkType::Lan
        );
        assert_eq!(
            detect_network_phase2(NetworkType::Wan, 3.0),
            NetworkType::Wan
        );
    }

    fn make_controller(initial_bitrate: u32, is_lan: bool, target_fps: u32) -> BitrateController {
        BitrateController::new(initial_bitrate, target_fps, is_lan)
    }

    #[test]
    fn lan_bypass_no_adjustment() {
        let mut ctrl = make_controller(10_000_000, true, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 20.0,
                frame_bytes: 500_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.bitrate_bps, 10_000_000);
        assert_eq!(decision.fps, 60);
    }

    #[test]
    fn bitrate_decrease_on_encode_overload() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 12.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.bitrate_bps, (10_000_000.0 * 0.85) as u32);
    }

    #[test]
    fn bitrate_floor_enforced() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        ctrl.set_bitrate_to_floor_for_test();
        let floor = ctrl.current_bitrate();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 12.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert!(decision.bitrate_bps >= floor);
    }

    #[test]
    fn bitrate_recovery_only_when_fps_at_target() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        ctrl.set_bitrate_to_floor_for_test();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 14.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        ctrl.evaluate();
        assert_eq!(ctrl.current_fps(), 30);
        let floor_bitrate = ctrl.current_bitrate();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 3.0,
                frame_bytes: 20_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.fps, 60);
        assert_eq!(decision.bitrate_bps, floor_bitrate);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 3.0,
                frame_bytes: 20_000,
                is_keyframe: false,
            });
        }
        let decision2 = ctrl.evaluate();
        assert!(decision2.bitrate_bps > floor_bitrate);
    }

    #[test]
    fn fps_decrease_after_bitrate_floor() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        ctrl.set_bitrate_to_floor_for_test();
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 14.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.fps, 30);
    }

    #[test]
    fn keyframes_excluded_from_size_evaluation() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 5.0,
                frame_bytes: 500_000,
                is_keyframe: true,
            });
        }
        let decision = ctrl.evaluate();
        assert_eq!(decision.bitrate_bps, 10_000_000);
    }

    #[test]
    fn idle_recovery_resets() {
        let mut ctrl = make_controller(10_000_000, false, 60);
        for _ in 0..40 {
            ctrl.record_frame(FrameStats {
                encode_ms: 12.0,
                frame_bytes: 50_000,
                is_keyframe: false,
            });
        }
        ctrl.evaluate();
        assert!(ctrl.current_bitrate() < 10_000_000);
        ctrl.on_idle_recovery();
        assert_eq!(ctrl.current_bitrate(), 10_000_000);
        assert_eq!(ctrl.current_fps(), 60);
    }
}
