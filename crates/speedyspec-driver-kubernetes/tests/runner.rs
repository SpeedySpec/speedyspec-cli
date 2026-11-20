//! The runner pod against the fake cluster: where it goes, what its Job holds, and that it never stays.

use std::time::Duration;

use serde_json::json;
use speedyspec_driver::{Workload, WorkloadKind};
use speedyspec_driver_kubernetes::{
    ConnectFlags, Connection, RunnerPod, RunnerSpec, connect, place_runner, runner_job,
};
use speedyspec_testkit::FakeCluster;

fn connection(cluster: &FakeCluster) -> Connection {
    let url = cluster.url();
    let flags = ConnectFlags {
        server: Some(&url),
        token_file: None,
        ca_cert: None,
        insecure_skip_tls_verify: false,
        context: None,
    };
    connect(&flags, Some(cluster.token()), &mut std::io::empty())
        .unwrap_or_else(|_| panic!("could not connect"))
}

fn workload(container: Option<&str>) -> Workload {
    Workload {
        kind: WorkloadKind::Deployment,
        name: "web".into(),
        root: "/srv/app".into(),
        container: container.map(str::to_owned),
    }
}

fn spec() -> RunnerSpec {
    RunnerSpec {
        image: "busybox@sha256:test".into(),
        deadline: Duration::from_secs(600),
        start_wait: Duration::from_secs(1),
        install: "shop".into(),
    }
}

/// The web Deployment, a pod of it on node n0 that is not ready, and a ready one on node n1 with two containers: the
/// sidecar first, then the app with the data volume (a subPath of a claim) and a service account token.
fn shop(cluster: &FakeCluster) {
    cluster.put(json!({"apiVersion": "v1", "kind": "Namespace", "metadata": {"name": "shop"}}));
    cluster.put(json!({"apiVersion": "apps/v1", "kind": "Deployment",
                       "metadata": {"name": "web", "namespace": "shop"},
                       "spec": {"selector": {"matchLabels": {"app": "web", "tier": "front"}}}}));
    let pod = |name: &str, node: &str, ready: &str| {
        json!({"apiVersion": "v1", "kind": "Pod",
               "metadata": {"name": name, "namespace": "shop", "labels": {"app": "web", "tier": "front"}},
               "spec": {"nodeName": node,
                        "securityContext": {"fsGroup": 33},
                        "containers": [
                            {"name": "sidecar", "image": "nginx", "volumeMounts": [{"name": "cache", "mountPath": "/cache"}]},
                            {"name": "app", "image": "wordpress", "securityContext": {"runAsUser": 33},
                             "env": [{"name": "WORDPRESS_DB_PASSWORD", "value": "hunter2-secret"}],
                             "volumeMounts": [
                                 {"name": "data", "mountPath": "/srv/app", "subPath": "site"},
                                 {"name": "kube-api-access-x", "mountPath": "/var/run/secrets/kubernetes.io/serviceaccount", "readOnly": true}]}],
                        "volumes": [
                            {"name": "data", "persistentVolumeClaim": {"claimName": "shop-data"}},
                            {"name": "cache", "emptyDir": {}},
                            {"name": "kube-api-access-x", "projected": {"sources": []}}]},
               "status": {"phase": "Running", "conditions": [{"type": "Ready", "status": ready}]}})
    };
    cluster.put(pod("web-0", "n0", "False"));
    cluster.put(pod("web-1", "n1", "True"));
    cluster.put(
        json!({"apiVersion": "v1", "kind": "Node", "metadata": {"name": "n1"},
                       "status": {"nodeInfo": {"architecture": "arm64"}}}),
    );
}

#[test]
fn the_runner_goes_beside_a_ready_pod_with_the_app_container_s_volumes() {
    let cluster = FakeCluster::start();
    shop(&cluster);
    let connection = connection(&cluster);
    let placement = place_runner(&connection, "shop", &workload(Some("app"))).unwrap();
    assert_eq!(placement.pod, "web-1");
    assert_eq!(placement.node, "n1");
    assert_eq!(placement.architecture, "arm64");

    let job = runner_job("shop", &placement, &spec(), "speedyspec-run-shop-abc123");
    let template = &job["spec"]["template"]["spec"];
    assert_eq!(job["metadata"]["labels"]["speedyspec.runner"], "1");
    assert_eq!(job["metadata"]["labels"]["speedyspec.install"], "shop");
    assert_eq!(job["spec"]["activeDeadlineSeconds"], 600);
    assert_eq!(job["spec"]["ttlSecondsAfterFinished"], 60);
    assert_eq!(job["spec"]["backoffLimit"], 0);
    assert_eq!(template["nodeName"], "n1");
    assert_eq!(template["restartPolicy"], "Never");
    assert_eq!(template["automountServiceAccountToken"], false);
    assert_eq!(template["securityContext"]["fsGroup"], 33);
    let container = &template["containers"][0];
    assert_eq!(template["containers"].as_array().unwrap().len(), 1);
    assert_eq!(container["image"], "busybox@sha256:test");
    assert_eq!(container["command"], json!(["sleep", "600"]));
    assert_eq!(container["securityContext"]["runAsUser"], 33);
    let mounts = container["volumeMounts"].as_array().unwrap();
    assert!(
        mounts.contains(&json!({"name": "data", "mountPath": "/srv/app", "subPath": "site"})),
        "{mounts:?}"
    );
    assert!(
        mounts.iter().any(|m| m["mountPath"] == "/speedyspec"),
        "{mounts:?}"
    );
    assert!(
        !mounts
            .iter()
            .any(|m| m["name"] == "cache" || m["name"] == "kube-api-access-x"),
        "{mounts:?}"
    );
    let volumes = template["volumes"].as_array().unwrap();
    assert!(
        volumes.contains(
            &json!({"name": "data", "persistentVolumeClaim": {"claimName": "shop-data"}})
        )
    );
    assert!(
        volumes.iter().any(|v| v["emptyDir"]["medium"] == "Memory"),
        "{volumes:?}"
    );
    // Archives go on a disk-backed emptyDir of their own, so an app with a read-only root filesystem still works.
    let work = volumes
        .iter()
        .find(|v| v["name"] == "speedyspec-work")
        .expect("a work volume");
    assert_eq!(work["emptyDir"], json!({}), "{work}");
    assert!(mounts.iter().any(|m| m["name"] == "speedyspec-work"
        && m["mountPath"] == speedyspec_driver_kubernetes::WORK_DIR));
    assert_eq!(volumes.len(), 3, "{volumes:?}");
    // Nothing of the app's environment, and no secret anywhere.
    assert!(!job.to_string().contains("hunter2-secret"));
    assert!(!job.to_string().contains("\"env\""));
}

#[test]
fn the_only_container_needs_no_name() {
    let cluster = FakeCluster::start();
    shop(&cluster);
    let connection = connection(&cluster);
    let error = place_runner(&connection, "shop", &workload(None)).unwrap_err();
    assert!(
        error.0.contains("several containers") && error.0.contains("sidecar"),
        "{}",
        error.0
    );
    let error = place_runner(&connection, "shop", &workload(Some("php"))).unwrap_err();
    assert!(error.0.contains("no container php"), "{}", error.0);
}

#[test]
fn no_ready_pod_is_refused_before_any_job() {
    let cluster = FakeCluster::start();
    shop(&cluster);
    let mut gone = cluster.object("v1", "Pod", Some("shop"), "web-1").unwrap();
    gone["status"]["conditions"][0]["status"] = json!("False");
    cluster.put(gone);
    let connection = connection(&cluster);
    let error = place_runner(&connection, "shop", &workload(Some("app"))).unwrap_err();
    assert!(
        error.0.contains("no ready pod") && error.0.contains("Deployment web"),
        "{}",
        error.0
    );
    assert!(cluster.objects_of("Job").is_empty());
    let error = place_runner(
        &connection,
        "shop",
        &Workload {
            name: "missing".into(),
            ..workload(Some("app"))
        },
    )
    .unwrap_err();
    assert!(error.0.contains("Deployment missing"), "{}", error.0);
}

fn binary() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "#!/bin/sh\n").unwrap();
    file
}

#[test]
fn the_job_is_deleted_when_its_pod_never_starts() {
    let cluster = FakeCluster::start();
    shop(&cluster);
    let connection = connection(&cluster);
    let placement = place_runner(&connection, "shop", &workload(Some("app"))).unwrap();
    let binary = binary();
    let error = match RunnerPod::start(
        &connection,
        "shop",
        &placement,
        binary.path(),
        "0".repeat(64).as_str(),
        &spec(),
    ) {
        Ok(_) => panic!("a runner pod that never ran was used"),
        Err(error) => error.0,
    };
    assert!(error.contains("did not start"), "{error}");
    assert!(
        cluster.objects_of("Job").is_empty(),
        "the Job was left behind"
    );
    assert!(
        cluster
            .writes()
            .iter()
            .any(|write| write.starts_with("PATCH") && write.contains("jobs")),
        "no Job was made: {:?}",
        cluster.writes()
    );
}

#[test]
fn an_image_that_does_not_pull_is_named() {
    let cluster = FakeCluster::start();
    shop(&cluster);
    let connection = connection(&cluster);
    let placement = place_runner(&connection, "shop", &workload(Some("app"))).unwrap();
    let binary = binary();
    let mut slow = spec();
    slow.start_wait = Duration::from_secs(20);
    let error = std::thread::scope(|scope| {
        scope.spawn(|| {
            // Plays the Job controller and the kubelet: the Job's pod appears and cannot pull its image.
            for _ in 0..200 {
                if let Some(job) = cluster.objects_of("Job").first() {
                    let name = job["metadata"]["name"].as_str().unwrap().to_owned();
                    cluster.put(json!({"apiVersion": "v1", "kind": "Pod",
                        "metadata": {"name": format!("{name}-x"), "namespace": "shop", "labels": {"job-name": name}},
                        "status": {"phase": "Pending", "containerStatuses": [{"name": "runner",
                            "state": {"waiting": {"reason": "ImagePullBackOff", "message": "pull access denied"}}}]}}));
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        match RunnerPod::start(
            &connection,
            "shop",
            &placement,
            binary.path(),
            "0".repeat(64).as_str(),
            &slow,
        ) {
            Ok(_) => panic!("a runner pod whose image did not pull was used"),
            Err(error) => error.0,
        }
    });
    assert!(
        error.contains("busybox@sha256:test") && error.contains("pull access denied"),
        "{error}"
    );
    assert!(
        cluster.objects_of("Job").is_empty(),
        "the Job was left behind"
    );
}
