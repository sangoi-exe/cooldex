use std::path::PathBuf;

use crate::ComputerUseError;
use crate::ComputerUseErrorCode;
use crate::ComputerUseOutput;
use crate::ComputerUseRequest;
use crate::ComputerUseRuntime;
use crate::DesktopSessionConfig;

const UNSUPPORTED_PLATFORM_MESSAGE: &str =
    "unsupported platform; Computer Use currently requires Linux x86_64";

#[derive(Debug, Clone)]
pub struct LocalComputerUseRuntimeConfig {
    pub session: DesktopSessionConfig,
    pub sky_binary_path: PathBuf,
}

impl Default for LocalComputerUseRuntimeConfig {
    fn default() -> Self {
        Self {
            session: DesktopSessionConfig::default(),
            sky_binary_path: PathBuf::from("sky"),
        }
    }
}

#[derive(Clone)]
pub struct LocalComputerUseRuntime;

impl LocalComputerUseRuntime {
    pub fn new(_config: LocalComputerUseRuntimeConfig) -> Self {
        Self
    }
}

impl ComputerUseRuntime for LocalComputerUseRuntime {
    fn execute(
        &self,
        _request: ComputerUseRequest,
    ) -> impl std::future::Future<Output = Result<ComputerUseOutput, ComputerUseError>> + Send {
        std::future::ready(Err(ComputerUseError::new(
            ComputerUseErrorCode::UnsupportedPlatform,
            UNSUPPORTED_PLATFORM_MESSAGE,
            /*retryable*/ false,
        )))
    }
}
