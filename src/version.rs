//! Version parsing and single-digit rollover calculation logic.
//!
//! Scheme: X.Y.Z
//! - X is unbounded (u64).
//! - Y (minor) and Z (patch) are single digits (0..=9).
//! - If within 24h: increment Z; if Z > 9 then Z = 0, Y += 1; if Y > 9 then Y = 0, X += 1.
//! - If >= 24h: increment Y, reset Z = 0; if Y > 9 then Y = 0, X += 1.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReleaseVersion {
    pub major: u64,
    pub minor: u8,
    pub patch: u8,
}

impl ReleaseVersion {
    pub fn new(major: u64, minor: u8, patch: u8) -> Option<Self> {
        if minor > 9 || patch > 9 {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    /// Parses a version string like "5.9", "5.9.0", "v6.0.0".
    pub fn parse(s: &str) -> Option<Self> {
        let trimmed = s.trim().trim_start_matches('v');
        let parts: Vec<&str> = trimmed.split('.').collect();
        match parts.as_slice() {
            [maj_str, min_str] => {
                let major = maj_str.parse::<u64>().ok()?;
                let minor = min_str.parse::<u8>().ok()?;
                Self::new(major, minor, 0)
            }
            [maj_str, min_str, pat_str] => {
                let major = maj_str.parse::<u64>().ok()?;
                let minor = min_str.parse::<u8>().ok()?;
                let patch = pat_str.parse::<u8>().ok()?;
                Self::new(major, minor, patch)
            }
            _ => None,
        }
    }

    /// Computes the next version according to the single-digit rollover scheme.
    pub fn next(&self, within_24h: bool) -> Self {
        let mut maj = self.major;
        let mut min = self.minor;
        let mut pat = self.patch;

        if within_24h {
            pat += 1;
            if pat > 9 {
                pat = 0;
                min += 1;
                if min > 9 {
                    min = 0;
                    maj += 1;
                }
            }
        } else {
            min += 1;
            pat = 0;
            if min > 9 {
                min = 0;
                maj += 1;
            }
        }

        Self {
            major: maj,
            minor: min,
            patch: pat,
        }
    }
}

impl std::fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}
