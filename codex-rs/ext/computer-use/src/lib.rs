mod extension;
mod protocol;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod runtime;
#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
#[path = "runtime_stub.rs"]
mod runtime;
mod server;
mod session;
mod sky;
mod vendor;

pub use extension::CODEX_COMPUTER_USE_MCP_BIN_ENV_VAR;
pub use extension::CODEX_COMPUTER_USE_OPENBOX_BIN_ENV_VAR;
pub use extension::CODEX_COMPUTER_USE_SKY_BIN_ENV_VAR;
pub use extension::CODEX_COMPUTER_USE_TEMP_ROOT_ENV_VAR;
pub use extension::CODEX_COMPUTER_USE_XVFB_BIN_ENV_VAR;
pub use extension::install;
pub use protocol::COMPUTER_USE_SERVER_NAME;
pub use protocol::ComputerUseError;
pub use protocol::ComputerUseErrorCode;
pub use protocol::ComputerUseOutput;
pub use protocol::ComputerUseRequest;
pub use protocol::DesktopEnvironment;
pub use protocol::IMAGE_DETAIL_META_KEY;
pub use protocol::IMAGE_DETAIL_ORIGINAL;
pub use protocol::SCREENSHOT_MIME_TYPE;
pub use protocol::SCREENSHOT_VIEWPORT_HEIGHT;
pub use protocol::SCREENSHOT_VIEWPORT_WIDTH;
pub use protocol::SKY_DEFAULT_MOUSE_SIZE_PX;
pub use protocol::SKY_DEFAULT_POST_ACTION_SLEEP_MS;
pub use protocol::SKY_DEFAULT_TIMEOUT_MS;
pub use protocol::ScreenshotPayload;
pub use runtime::LocalComputerUseRuntime;
pub use runtime::LocalComputerUseRuntimeConfig;
pub use server::ComputerUseRuntime;
pub use server::ComputerUseServer;
pub use server::computer_use_tools;
pub use server::dispatch_tool_call;
pub use session::DesktopCommandPaths;
pub use session::DesktopSession;
pub use session::DesktopSessionConfig;
pub use sky::SkyInvocation;
pub use sky::parse_screenshot_stdout;
pub use sky::screenshot_content_block;
pub use sky::sky_invocation_for_request;
pub use sky::validated_screenshot_payload_from_jpeg;
pub use vendor::PROVENANCE_REL_PATH;
pub use vendor::VENDORED_ARTIFACTS;
pub use vendor::VendoredArtifact;
pub use vendor::vendored_artifact_path;
pub use vendor::vendored_openai_root;

#[cfg(test)]
#[path = "vendor_tests.rs"]
mod vendor_tests;

#[cfg(test)]
#[path = "server_tests.rs"]
mod server_tests;

#[cfg(test)]
#[path = "sky_tests.rs"]
mod sky_tests;

#[cfg(test)]
#[path = "session_tests.rs"]
mod session_tests;

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod runtime_tests;

#[cfg(test)]
#[path = "extension_tests.rs"]
mod extension_tests;
