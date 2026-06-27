/// Vendor-neutral call-signaling gate.
///
/// Each conference vendor ships an adapter implementing this trait; the detector
/// ORs adapter results into the entry gate (`has_conn = turn || (signaling_active
/// && bc)`). v1 wires only the Meet adapter (Google-CIDR TCP check); a second
/// vendor adapter arrives together with the `Vec<dyn CallSignalingPort>`
/// aggregator when a real second caller exists.
pub trait CallSignalingPort: Send + Sync {
    /// `true` when an active call-signaling TCP connection for this vendor is
    /// observed (e.g. HTTPS/WSS signaling to Google media-server IPs).
    fn is_call_signaling_active(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubSignaling {
        active: bool,
    }

    impl CallSignalingPort for StubSignaling {
        fn is_call_signaling_active(&self) -> bool {
            self.active
        }
    }

    #[test]
    fn stub_returns_configured_value() {
        assert!(StubSignaling { active: true }.is_call_signaling_active());
        assert!(!StubSignaling { active: false }.is_call_signaling_active());
    }
}
