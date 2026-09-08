//! Host support contract.
//!
//! v1 supports macOS arm64 and nothing else. The sandbox profile, the pinned sidecar, and the
//! recorded command profiles are all platform-specific, so running elsewhere would produce
//! evidence that means nothing.

/// The only supported operating system.
pub const SUPPORTED_OS: &str = "macos";
/// The only supported architecture.
pub const SUPPORTED_ARCH: &str = "aarch64";

/// A host that passed the support probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostSupport {
    /// Operating system name, as reported by the target.
    pub os: &'static str,
    /// Architecture name, as reported by the target.
    pub arch: &'static str,
}

/// Why a host was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HostError {
    /// The host is not macOS arm64.
    #[error(
        "unsupported host `{os}/{arch}`; agent-jit v1 supports `{SUPPORTED_OS}/{SUPPORTED_ARCH}` only"
    )]
    Unsupported {
        /// Operating system that was probed.
        os: String,
        /// Architecture that was probed.
        arch: String,
    },
}

impl HostError {
    /// Stable machine-readable code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Unsupported { .. } => "host_unsupported",
        }
    }
}

impl HostSupport {
    /// Probes an explicit operating system and architecture pair.
    ///
    /// Taking both as arguments keeps the refusal path testable on the supported host, where a
    /// `cfg!` check alone could never be exercised.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Unsupported`] for anything but macOS arm64.
    pub fn probe(os: &str, arch: &str) -> Result<Self, HostError> {
        if os == SUPPORTED_OS && arch == SUPPORTED_ARCH {
            Ok(Self {
                os: SUPPORTED_OS,
                arch: SUPPORTED_ARCH,
            })
        } else {
            Err(HostError::Unsupported {
                os: os.to_owned(),
                arch: arch.to_owned(),
            })
        }
    }

    /// Probes the host this binary was built for.
    ///
    /// # Errors
    ///
    /// Returns [`HostError::Unsupported`] when the build target is not macOS arm64.
    pub fn current() -> Result<Self, HostError> {
        Self::probe(std::env::consts::OS, std::env::consts::ARCH)
    }
}
