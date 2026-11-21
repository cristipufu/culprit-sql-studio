use super::*;
use clap::Parser;
use culprit_core::{Engine, telemetry::*};
#[test]
fn group_replication_collection_requires_explicit_native_instance_and_keeps_bounded_point_selection() {
    let directory = std::env::temp_dir().join(format!(
        "culprit-gr-selection-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("resource.json");
    let resource = SharedResourceIdentity {
        engine: Engine::Mysql,
        kind: SharedResourceKind::Instance,
        namespace: NativeIdentity::new("native-server"),
        native: NativeIdentity::new("original-observer"),
    };
    std::fs::write(&path, serde_json::to_vec(&resource).unwrap()).unwrap();
    let base = ["culprit", "collect", "once", "fixture", "--collection", "group-replication", "--output", "capture"];
    let cli = crate::Cli::try_parse_from(base.into_iter().chain(["--resource", path.to_str().unwrap()])).unwrap();
    let crate::Command::Collect(CollectCommand::Once(args)) = cli.command else { panic!() };
    let request = args.request().unwrap();
    assert_eq!(request.subject, CollectionSubject::Shared { resource });
    assert_eq!(request.selection, CollectionSelection::Snapshot { dataset: SnapshotDataset::GroupReplication });
    let cli = crate::Cli::try_parse_from(base).unwrap();
    let crate::Command::Collect(CollectCommand::Once(args)) = cli.command else { panic!() };
    assert!(args.request().is_err());
    let cli = crate::Cli::try_parse_from(base.into_iter().chain([
        "--resource",
        path.to_str().unwrap(),
        "--from",
        "2020-01-01T00:00:00Z",
        "--until",
        "2020-01-01T01:00:00Z",
    ]))
    .unwrap();
    let crate::Command::Collect(CollectCommand::Once(args)) = cli.command else { panic!() };
    assert!(args.request().is_err());
    std::fs::remove_dir_all(directory).unwrap();
}
