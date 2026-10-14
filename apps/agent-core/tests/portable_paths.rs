use kynxa_agent_core_native::storage::{administration, extensions};
use serde_json::{Value, json};
use std::fs;
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kynxa-portable-paths-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("first location")).unwrap();
        fs::create_dir_all(root.join("moved location")).unwrap();
        Self { root }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        assert!(self.root.starts_with(std::env::temp_dir()));
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn portable_data_follows_the_application_and_explicit_roots_win() {
    let fixture = Fixture::new();
    let first = fixture.root.join("first location");
    let moved = fixture.root.join("moved location");
    let model_home = |arguments: Value| {
        administration::apply("storage.administration.modelHome", &arguments).unwrap()
    };
    assert_eq!(
        model_home(json!({"applicationRoot":first})),
        json!(first.join("Data").join("Models"))
    );
    let portable = b"\xef\xbb\xbf{\"version\":1,\"dataRoot\":\"./Shared Data\"}";
    fs::write(first.join("storage.json"), portable).unwrap();
    fs::write(moved.join("storage.json"), portable).unwrap();
    assert_eq!(
        model_home(json!({"applicationRoot":first})),
        json!(first.join("Shared Data").join("Models"))
    );
    assert_eq!(
        model_home(json!({"applicationRoot":moved})),
        json!(moved.join("Shared Data").join("Models"))
    );
    let chosen = fixture.root.join("chosen data");
    fs::write(
        moved.join("storage.json"),
        serde_json::to_vec(&json!({"version":1,"dataRoot":chosen})).unwrap(),
    )
    .unwrap();
    assert_eq!(
        model_home(json!({"applicationRoot":moved})),
        json!(chosen.join("Models"))
    );
    assert_eq!(
        model_home(json!({"applicationRoot":moved,"dataHome":"Override"})),
        json!(moved.join("Override").join("Models"))
    );
    assert_eq!(
        model_home(json!({"applicationRoot":moved,"dataHome":"Override","modelHome":"Own Models"})),
        json!(moved.join("Own Models"))
    );
}

#[test]
fn portable_extensions_follow_the_pointer_and_preserve_isolated_overrides() {
    let fixture = Fixture::new();
    let first = fixture.root.join("first location");
    let moved = fixture.root.join("moved location");
    let extension_home = |arguments: Value| {
        extensions::apply("storage.extensions.home", &arguments).unwrap()
    };
    assert_eq!(
        extension_home(json!({"pointer":first.join("extensions.json"),"fallback":first.join("Extensions")})),
        json!(first.join("Extensions"))
    );
    let portable = b"\xef\xbb\xbf{\"version\":1,\"extensionRoot\":\"User Tools\"}";
    fs::write(first.join("extensions.json"), portable).unwrap();
    fs::write(moved.join("extensions.json"), portable).unwrap();
    for application in [&first, &moved] {
        assert_eq!(
            extension_home(json!({"pointer":application.join("extensions.json"),"fallback":application.join("Extensions")})),
            json!(application.join("User Tools"))
        );
    }
    let isolated = fixture.root.join("isolated data");
    assert_eq!(
        extension_home(json!({"pointer":moved.join("extensions.json"),"isolated":true,"fallback":isolated})),
        json!(isolated)
    );
    assert_eq!(
        extension_home(json!({"applicationRoot":moved,"explicitRoot":"Override Tools","pointer":first.join("extensions.json")})),
        json!(moved.join("Override Tools"))
    );
}
