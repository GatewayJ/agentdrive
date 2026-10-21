use agentdrive_config::{ConfigStore, RemoteConfig, SettingsUpdate, initialize};
use agentdrive_model::{ErrorCode, Principal};
use serde_json::{Value, json};
use std::{fs, sync::Arc};

fn owner() -> Principal {
    "owner".to_owned().try_into().unwrap()
}
fn setup() -> (tempfile::TempDir, Arc<ConfigStore>) {
    let directory = tempfile::tempdir().unwrap();
    let path = initialize(directory.path(), "127.0.0.1:7421".parse().unwrap()).unwrap();
    let store = ConfigStore::open(&path).unwrap();
    (directory, store)
}

#[test]
fn opencsg_token_is_private_and_only_reused_for_the_same_destination() {
    let (_directory, store) = setup();
    let settings = store.settings(&owner()).unwrap();
    let mut runtime = settings.runtime;
    runtime.remote = Some(
        serde_json::from_value(json!({
            "backend":"opencsg", "endpoint":"https://opencsg.example.com",
            "repository":"owner/storage", "token":"private-test-token"
        }))
        .unwrap(),
    );
    let saved = store
        .update(
            &owner(),
            SettingsUpdate {
                revision: settings.revision,
                runtime,
            },
        )
        .unwrap();
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("private-test-token")
    );
    assert!(!format!("{:?}", store.live().load()).contains("private-test-token"));
    let mut runtime = saved.runtime.clone();
    let RemoteConfig::OpenCsg(config) = runtime.remote.as_mut().unwrap() else {
        panic!("OpenCSG settings");
    };
    assert!(config.token.is_empty());
    config.prefix = "agentdrive/other".into();
    let saved = store
        .update(
            &owner(),
            SettingsUpdate {
                revision: saved.revision,
                runtime,
            },
        )
        .unwrap();
    let reopened = ConfigStore::open(&saved.config_path).unwrap();
    let snapshot = reopened.live().load();
    let RemoteConfig::OpenCsg(config) = snapshot.values.remote.as_ref().unwrap() else {
        panic!("OpenCSG settings");
    };
    assert_eq!(config.token, "private-test-token");
    for field in ["endpoint", "repository"] {
        let mut update = serde_json::to_value(&saved.runtime).unwrap();
        update["remote"][field] = if field == "endpoint" {
            json!("https://other.example.com")
        } else {
            json!("other/storage")
        };
        let before = fs::read(&saved.config_path).unwrap();
        let error = store
            .update(
                &owner(),
                SettingsUpdate {
                    revision: saved.revision.clone(),
                    runtime: serde_json::from_value(update).unwrap(),
                },
            )
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(fs::read(&saved.config_path).unwrap(), before);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&saved.config_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn remote_configuration_accepts_legacy_s3_and_rejects_unsafe_opencsg_destinations() {
    let legacy: RemoteConfig =
        serde_json::from_value(json!({"bucket":"existing", "endpoint":null})).unwrap();
    assert!(matches!(legacy, RemoteConfig::S3(_)));
    legacy.validate().unwrap();
    let base = json!({"backend":"opencsg","endpoint":"https://example.com","repository":"owner/repo","token":"token"});
    let good: RemoteConfig = serde_json::from_value(base.clone()).unwrap();
    good.validate().unwrap();
    for (field, invalid) in [
        ("endpoint", "https://token@example.com"),
        ("endpoint", "https://example.com/api"),
        ("endpoint", "http://example.com"),
        ("endpoint", "https://example.com?token=x"),
        ("repository", "../storage"),
        ("repository", "owner/repo/extra"),
        ("prefix", "../data"),
        ("branch", "main..branch"),
        ("token", "token\nheader"),
    ] {
        let mut value = base.clone();
        value[field] = invalid.into();
        let config: RemoteConfig = serde_json::from_value(value).unwrap();
        assert!(config.validate().is_err(), "{field}");
    }
}

#[test]
fn file_updates_publish_complete_snapshots_and_preserve_in_flight_readers() {
    let (_directory, store) = setup();
    let before = store.live().load();
    let settings = store.settings(&owner()).unwrap();
    let mut config: Value =
        serde_json::from_slice(&fs::read(&settings.config_path).unwrap()).unwrap();
    assert_eq!(config.as_object().unwrap().len(), 5);
    config["cache"] = json!({"max_bytes":3221225472u64});
    config["allowed_origins"] = json!(["https://example.com"]);
    fs::write(&settings.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(store.reload().unwrap());
    let after = store.live().load();
    assert_eq!(after.values.cache.max_bytes, 3 * 1024 * 1024 * 1024);
    assert_eq!(before.values.cache.max_bytes, 2 * 1024 * 1024 * 1024);
    assert_eq!(after.values.allowed_origins, vec!["https://example.com"]);
    assert!(before.values.allowed_origins.is_empty());
    assert_ne!(after.revision, before.revision);
    assert!(!store.reload().unwrap());
}

#[test]
fn saved_cache_configuration_preserves_capacity_and_writes_one_field() {
    let (_directory, store) = setup();
    let settings = store.settings(&owner()).unwrap();
    let mut config: Value =
        serde_json::from_slice(&fs::read(&settings.config_path).unwrap()).unwrap();
    config["cache"] = json!({
        "max_bytes":134217728,
        "target_bytes":100663296,
        "min_free_bytes":33554432
    });
    fs::write(&settings.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(store.reload().unwrap());
    let reopened = ConfigStore::open(&settings.config_path).unwrap();
    let loaded = reopened.settings(&owner()).unwrap();
    assert_eq!(loaded.runtime.cache.max_bytes, 128 * 1024 * 1024);
    assert_eq!(
        serde_json::to_value(&loaded.runtime.cache).unwrap(),
        json!({"max_bytes":134217728})
    );
    reopened
        .update(
            &owner(),
            SettingsUpdate {
                revision: loaded.revision,
                runtime: loaded.runtime,
            },
        )
        .unwrap();
    let saved: Value = serde_json::from_slice(&fs::read(&settings.config_path).unwrap()).unwrap();
    assert_eq!(saved["cache"], json!({"max_bytes":134217728}));
}

#[test]
fn invalid_files_and_startup_changes_retain_all_active_values_until_repaired() {
    let (_directory, store) = setup();
    let settings = store.settings(&owner()).unwrap();
    let original = fs::read(&settings.config_path).unwrap();
    let config: Value = serde_json::from_slice(&original).unwrap();
    let mut unknown = config.clone();
    unknown["unknown_field"] = json!(true);
    let mut cache = config.clone();
    cache["cache"] = json!({"max_bytes":0});
    let mut nested = config.clone();
    nested["cache"] = json!({"unknown_field":1});
    let mut origins = config.clone();
    origins["allowed_origins"] = json!(["https://example.com/path"]);
    let mut startup = config.clone();
    startup["listen"] = json!("127.0.0.1:7422");
    startup["cache"] = json!({"max_bytes":3221225472u64});
    for invalid in [unknown, cache, nested, origins, startup] {
        fs::write(&settings.config_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        assert!(store.reload().is_err());
        let current = store.live().load();
        assert_eq!(current.revision, settings.revision);
        assert_eq!(current.values, settings.runtime);
        assert!(current.reload_error.is_some());
    }
    for bytes in [b"{".to_vec(), vec![b' '; 1024 * 1024 + 1]] {
        fs::write(&settings.config_path, bytes).unwrap();
        assert!(store.reload().is_err());
        assert_eq!(store.live().load().values, settings.runtime);
    }
    fs::remove_file(&settings.config_path).unwrap();
    assert!(store.reload().is_err());
    fs::write(&settings.config_path, original).unwrap();
    assert!(store.reload().unwrap());
    assert!(store.live().load().reload_error.is_none());
}

#[test]
fn saves_survive_restart_and_reject_stale_or_unloaded_file_edits() {
    let (_directory, store) = setup();
    let settings = store.settings(&owner()).unwrap();
    let mut runtime = settings.runtime.clone();
    runtime.cache.max_bytes = 128 * 1024 * 1024;
    let saved = store
        .update(
            &owner(),
            SettingsUpdate {
                revision: settings.revision.clone(),
                runtime: runtime.clone(),
            },
        )
        .unwrap();
    assert_ne!(saved.revision, settings.revision);
    assert_eq!(store.live().load().values, runtime);
    assert_eq!(
        ConfigStore::open(&saved.config_path)
            .unwrap()
            .live()
            .load()
            .values,
        runtime
    );
    assert_eq!(
        store
            .update(
                &owner(),
                SettingsUpdate {
                    revision: settings.revision,
                    runtime: runtime.clone()
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    let mut bytes = fs::read(&saved.config_path).unwrap();
    bytes.push(b'\n');
    fs::write(&saved.config_path, &bytes).unwrap();
    assert_eq!(
        store
            .update(
                &owner(),
                SettingsUpdate {
                    revision: saved.revision,
                    runtime
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::Conflict
    );
    assert_eq!(fs::read(&saved.config_path).unwrap(), bytes);
    assert!(store.reload().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&saved.config_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn administrator_access_is_independent_of_workspace_access() {
    let (_directory, store) = setup();
    let visitor = "visitor".to_owned().try_into().unwrap();
    assert_eq!(
        store.settings(&visitor).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    let settings = store.settings(&owner()).unwrap();
    assert_eq!(
        store
            .update(
                &visitor,
                SettingsUpdate {
                    revision: settings.revision,
                    runtime: settings.runtime
                }
            )
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let mut config: Value =
        serde_json::from_slice(&fs::read(&settings.config_path).unwrap()).unwrap();
    config["administrators"] = json!(["visitor"]);
    fs::write(&settings.config_path, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(store.reload().is_err());
    let restarted = ConfigStore::open(&settings.config_path).unwrap();
    assert!(restarted.settings(&visitor).is_ok());
    assert_eq!(
        restarted.settings(&owner()).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
}

#[test]
fn concurrent_saves_accept_one_revision_and_readers_never_mix_versions() {
    let (_directory, store) = setup();
    let initial = store.settings(&owner()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(8));
    std::thread::scope(|scope| {
        let mut writers = Vec::new();
        for index in 0..8 {
            let store = store.clone();
            let barrier = barrier.clone();
            let revision = initial.revision.clone();
            let mut runtime = initial.runtime.clone();
            runtime.cache.max_bytes = 100 + index;
            runtime.allowed_origins = vec![format!("https://cache-{}.example", 100 + index)];
            writers.push(scope.spawn(move || {
                barrier.wait();
                store.update(&owner(), SettingsUpdate { revision, runtime })
            }));
        }
        let results: Vec<_> = writers
            .into_iter()
            .map(|writer| writer.join().unwrap())
            .collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(
            results
                .iter()
                .filter_map(|result| result.as_ref().err())
                .all(|error| error.code == ErrorCode::Conflict)
        );
    });
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let live = store.live();
            scope.spawn(move || {
                for _ in 0..10000 {
                    let snapshot = live.load();
                    assert_eq!(
                        snapshot.values.allowed_origins,
                        vec![format!(
                            "https://cache-{}.example",
                            snapshot.values.cache.max_bytes
                        )]
                    );
                }
            });
        }
        for index in 0..20 {
            let settings = store.settings(&owner()).unwrap();
            let mut runtime = settings.runtime;
            runtime.cache.max_bytes = index + 100;
            runtime.allowed_origins = vec![format!("https://cache-{}.example", index + 100)];
            store
                .update(
                    &owner(),
                    SettingsUpdate {
                        revision: settings.revision,
                        runtime,
                    },
                )
                .unwrap();
        }
    });
}

#[test]
fn s3_settings_persist_secrets_without_returning_them_and_preserve_blank_passwords() {
    let (_directory, store) = setup();
    let settings = store.settings(&owner()).unwrap();
    let mut runtime = settings.runtime;
    runtime.remote = Some(serde_json::from_value(json!({
        "bucket":"agentdrive-test", "endpoint":"https://s3.example.com",
        "credentials":{"access_key_id":"test-access", "secret_access_key":"test-secret", "session_token":"test-session"}
    })).unwrap());
    let saved = store
        .update(
            &owner(),
            SettingsUpdate {
                revision: settings.revision,
                runtime,
            },
        )
        .unwrap();
    let response = serde_json::to_string(&saved).unwrap();
    assert!(!response.contains("test-secret"));
    assert!(!response.contains("test-session"));
    assert!(!format!("{:?}", store.live().load()).contains("test-secret"));
    let mut runtime = saved.runtime;
    let RemoteConfig::S3(config) = runtime.remote.as_mut().unwrap() else {
        panic!("S3 settings");
    };
    config.region = "eu-west-1".into();
    let saved = store
        .update(
            &owner(),
            SettingsUpdate {
                revision: saved.revision,
                runtime,
            },
        )
        .unwrap();
    let restarted = ConfigStore::open(&saved.config_path).unwrap();
    let snapshot = restarted.live().load();
    let RemoteConfig::S3(config) = snapshot.values.remote.as_ref().unwrap() else {
        panic!("S3 settings");
    };
    let credentials = config.credentials.as_ref().unwrap();
    assert_eq!(credentials.secret_access_key, "test-secret");
    assert_eq!(credentials.session_token.as_deref(), Some("test-session"));
    let mut runtime = saved.runtime;
    let RemoteConfig::S3(config) = runtime.remote.as_mut().unwrap() else {
        panic!("S3 settings");
    };
    config.credentials = None;
    store
        .update(
            &owner(),
            SettingsUpdate {
                revision: saved.revision,
                runtime,
            },
        )
        .unwrap();
    assert!(
        !fs::read_to_string(&saved.config_path)
            .unwrap()
            .contains("test-secret")
    );
}
