use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendoredArtifact {
    pub relative_path: &'static str,
    pub bytes: u64,
    pub sha256: &'static str,
}

pub const PROVENANCE_REL_PATH: &str = "PROVENANCE.json";

pub const VENDORED_ARTIFACTS: &[VendoredArtifact] = &[
    VendoredArtifact {
        relative_path: "sky/0.6.2/bin/linux/sky_linux_x64",
        bytes: 2_909_128,
        sha256: "6ab59cabc5b817791c2d58d643d3046c6617b1d4aa5d6e01b956c7c36818011d",
    },
    VendoredArtifact {
        relative_path: "sky/0.6.2/package.json",
        bytes: 1_103,
        sha256: "6fcd0ef7d0b746dfbeedce74a862d88c5bbf6c5e595c20862feb53265b603b3f",
    },
    VendoredArtifact {
        relative_path: "sky/0.6.2/README.md",
        bytes: 5_716,
        sha256: "38edfba326c9ea3c36ac78d38b228a35f7a1ccafd4a762817deae78ce7acf80d",
    },
    VendoredArtifact {
        relative_path: "sky/0.6.2/docs/sky-full-desktop-api.md",
        bytes: 2_940,
        sha256: "a8dc0d1ccbe9c9419972e9858e13dd16b97b508b6ba8a78a23140ef0d0f4fbd0",
    },
    VendoredArtifact {
        relative_path: "sky/0.6.2/reference/linux/sky_linux.js",
        bytes: 777,
        sha256: "8c95f7b93eb7b69351b7ab87e19ebbed29035dd5a043c2b058cf0fff3ea905df",
    },
    VendoredArtifact {
        relative_path: "sky/0.6.2/reference/linux/get_screenshot.js",
        bytes: 621,
        sha256: "bbd1595aaced28b467a6e58b686577c0f8a8abb5fc42387c392ccd164a30bdda",
    },
    VendoredArtifact {
        relative_path: "sky/0.6.2/reference/linux/post_action_sleep.js",
        bytes: 389,
        sha256: "ce1c25bacf9c7ecb344fa1f4a9400e2a63ec1855d429cfad1a1241365f48e452",
    },
    VendoredArtifact {
        relative_path: "sky/0.6.2/reference/full-desktop/Options.d.ts",
        bytes: 391,
        sha256: "d35e0d89f0e4642309e56cb603983d2aa45e6d6d3cf8560d5810a04ab92baf0f",
    },
    VendoredArtifact {
        relative_path: "computer-use-plugin/26.727.51351/.codex-plugin/plugin.json",
        bytes: 1_730,
        sha256: "e81279cb0e9939c0edf2936b626d4c39a4a57333219d589c8e3d33758adbb50a",
    },
    VendoredArtifact {
        relative_path: "computer-use-plugin/26.727.51351/skills/computer-use/SKILL.md",
        bytes: 1_619,
        sha256: "0be0ff2686142b486597cbe0ad2050007a32bc0211c6c605e0a12ce9b00876ad",
    },
    VendoredArtifact {
        relative_path: "computer-use-plugin/26.727.51351/docs/api.md",
        bytes: 8_207,
        sha256: "cfb5535a75b568e7325ce1279ef703fb67c490b75ae9c052199fc0fd036d0725",
    },
    VendoredArtifact {
        relative_path: "computer-use-plugin/26.727.51351/docs/guidance.md",
        bytes: 14_166,
        sha256: "81b412ae2e9aae65aeef0e60517b32d51ce2c91ba65120d8139d416933a56676",
    },
    VendoredArtifact {
        relative_path: "computer-use-plugin/26.727.51351/docs/confirmations.md",
        bytes: 4_722,
        sha256: "ddb0c90c80344b1dac8d0c5f97e4a827532391e25502f1f990da28a0e414fd16",
    },
    VendoredArtifact {
        relative_path: "computer-use-plugin/26.727.51351/scripts/computer-use-client.mjs",
        bytes: 10_638,
        sha256: "c5e8b73f10ecc47ad1be38f14ba6b79cd3af06fcd86637f7b6b857ae209a686e",
    },
];

pub fn vendored_openai_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("vendor")
        .join("openai")
}

pub fn vendored_artifact_path(relative_path: &str) -> PathBuf {
    vendored_openai_root().join(relative_path)
}
