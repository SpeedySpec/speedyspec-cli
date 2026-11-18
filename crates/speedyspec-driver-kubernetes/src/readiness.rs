//! Is this object ready? Rules over the object as the cluster returns it; nothing here talks
//! to a cluster.

use std::time::Duration;

use serde_json::Value as Json;

use crate::manifest::ResourceId;

/// Whether an installed object is doing what it was installed to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    /// Not yet, and why, in a few words.
    NotYet(String),
    /// It will not become ready by waiting.
    Failed(String),
}

/// The kinds with a built-in rule.
pub const KNOWN_KINDS: [&str; 5] = ["Deployment", "StatefulSet", "DaemonSet", "Job", "Pod"];

/// Whether `object` of `kind` is ready. With `condition`, ready means that status condition is
/// `True`, whatever the kind.
pub fn readiness(kind: &str, object: &Json, condition: Option<&str>) -> Readiness {
    if let Some(condition) = condition {
        return match condition_status(object, condition) {
            Some("True") => Readiness::Ready,
            Some(other) => Readiness::NotYet(format!("condition {condition} is {other}")),
            None => Readiness::NotYet(format!("condition {condition} is not reported yet")),
        };
    }
    let status = &object["status"];
    let count = |value: &Json| value.as_u64().unwrap_or(0);
    let seen = count(&status["observedGeneration"]) >= count(&object["metadata"]["generation"]);
    let unseen = || Readiness::NotYet("not yet seen by its controller".to_owned());
    let short =
        |have: u64, want: u64, what: &str| Readiness::NotYet(format!("{have} of {want} {what}"));
    match kind {
        "Deployment" => {
            let want = object["spec"]["replicas"].as_u64().unwrap_or(1);
            let (updated, available) = (
                count(&status["updatedReplicas"]),
                count(&status["availableReplicas"]),
            );
            if !seen {
                unseen()
            } else if updated < want {
                short(updated, want, "updated")
            } else if available < want {
                short(available, want, "available")
            } else {
                Readiness::Ready
            }
        }
        "StatefulSet" => {
            let want = object["spec"]["replicas"].as_u64().unwrap_or(1);
            let ready = count(&status["readyReplicas"]);
            if !seen {
                unseen()
            } else if ready < want {
                short(ready, want, "ready")
            } else if status["currentRevision"] != status["updateRevision"] {
                Readiness::NotYet("update in progress".to_owned())
            } else {
                Readiness::Ready
            }
        }
        "DaemonSet" => {
            let want = count(&status["desiredNumberScheduled"]);
            let (ready, updated) = (
                count(&status["numberReady"]),
                count(&status["updatedNumberScheduled"]),
            );
            if !seen {
                unseen()
            } else if updated < want {
                short(updated, want, "updated")
            } else if ready < want {
                short(ready, want, "ready")
            } else {
                Readiness::Ready
            }
        }
        "Job" => {
            if condition_status(object, "Complete") == Some("True") {
                Readiness::Ready
            } else if condition_status(object, "Failed") == Some("True") {
                Readiness::Failed(
                    condition_message(object, "Failed")
                        .unwrap_or("the job failed")
                        .to_owned(),
                )
            } else {
                Readiness::NotYet("still running".to_owned())
            }
        }
        "Pod" => match status["phase"].as_str() {
            Some("Succeeded") => Readiness::Ready,
            Some("Failed") => Readiness::Failed(
                status["message"]
                    .as_str()
                    .unwrap_or("the pod failed")
                    .to_owned(),
            ),
            _ if condition_status(object, "Ready") == Some("True") => Readiness::Ready,
            Some(phase) => Readiness::NotYet(phase.to_owned()),
            None => Readiness::NotYet("not started".to_owned()),
        },
        other => Readiness::Failed(format!("no readiness rule for {other}; give a condition")),
    }
}

fn find_condition<'a>(object: &'a Json, kind: &str) -> Option<&'a Json> {
    object["status"]["conditions"]
        .as_array()?
        .iter()
        .find(|condition| condition["type"] == kind)
}

fn condition_status<'a>(object: &'a Json, kind: &str) -> Option<&'a str> {
    find_condition(object, kind)?["status"].as_str()
}

fn condition_message<'a>(object: &'a Json, kind: &str) -> Option<&'a str> {
    find_condition(object, kind)?["message"].as_str()
}

/// What an install waits on when the plugin does not say: its workloads.
pub fn default_wait(resources: &[ResourceId]) -> Vec<ResourceId> {
    resources
        .iter()
        .filter(|id| {
            matches!(
                id.kind.as_str(),
                "Deployment" | "StatefulSet" | "DaemonSet" | "Job"
            )
        })
        .cloned()
        .collect()
}

/// A duration such as `90s`, `10m` or `1h`.
pub fn parse_timeout(text: &str) -> Result<Duration, String> {
    let wrong = || format!("\"{text}\" is not a duration; write for example 90s, 10m or 1h");
    let unit_at = text.len().checked_sub(1).ok_or_else(wrong)?;
    if !text.is_char_boundary(unit_at) {
        return Err(wrong());
    }
    let (number, unit) = text.split_at(unit_at);
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(wrong());
    }
    let amount: u64 = number.parse().map_err(|_| wrong())?;
    let seconds = match unit {
        "s" => Some(amount),
        "m" => amount.checked_mul(60),
        "h" => amount.checked_mul(3600),
        _ => None,
    }
    .filter(|seconds| *seconds > 0)
    .ok_or_else(wrong)?;
    Ok(Duration::from_secs(seconds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn not_yet(readiness: Readiness) -> String {
        match readiness {
            Readiness::NotYet(reason) => reason,
            other => panic!("expected NotYet, got {other:?}"),
        }
    }

    fn failed(readiness: Readiness) -> String {
        match readiness {
            Readiness::Failed(reason) => reason,
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn deployment_rules() {
        let deployment = |status: Json| json!({"metadata": {"generation": 2}, "spec": {"replicas": 2}, "status": status});
        let ready = json!({"observedGeneration": 2, "updatedReplicas": 2, "availableReplicas": 2});
        assert_eq!(
            readiness("Deployment", &deployment(ready), None),
            Readiness::Ready
        );

        let unavailable =
            json!({"observedGeneration": 2, "updatedReplicas": 2, "availableReplicas": 0});
        assert_eq!(
            not_yet(readiness("Deployment", &deployment(unavailable), None)),
            "0 of 2 available"
        );

        let rolling =
            json!({"observedGeneration": 2, "updatedReplicas": 1, "availableReplicas": 2});
        assert_eq!(
            not_yet(readiness("Deployment", &deployment(rolling), None)),
            "1 of 2 updated"
        );

        let behind = json!({"observedGeneration": 1, "updatedReplicas": 2, "availableReplicas": 2});
        assert_eq!(
            not_yet(readiness("Deployment", &deployment(behind), None)),
            "not yet seen by its controller"
        );

        let no_status = json!({"metadata": {"generation": 1}, "spec": {"replicas": 1}});
        assert!(matches!(
            readiness("Deployment", &no_status, None),
            Readiness::NotYet(_)
        ));

        // Replicas default to one.
        let defaulted = json!({"metadata": {"generation": 1}, "spec": {},
            "status": {"observedGeneration": 1, "updatedReplicas": 1, "availableReplicas": 1}});
        assert_eq!(readiness("Deployment", &defaulted, None), Readiness::Ready);

        // Scaled to nothing is ready with nothing.
        let zero = json!({"metadata": {"generation": 1}, "spec": {"replicas": 0},
            "status": {"observedGeneration": 1}});
        assert_eq!(readiness("Deployment", &zero, None), Readiness::Ready);
    }

    #[test]
    fn statefulset_rules() {
        let set = |status: Json| json!({"metadata": {"generation": 1}, "spec": {"replicas": 3}, "status": status});
        let ready = json!({"observedGeneration": 1, "readyReplicas": 3,
                           "currentRevision": "a", "updateRevision": "a"});
        assert_eq!(
            readiness("StatefulSet", &set(ready), None),
            Readiness::Ready
        );
        let partial = json!({"observedGeneration": 1, "readyReplicas": 1,
                             "currentRevision": "a", "updateRevision": "a"});
        assert_eq!(
            not_yet(readiness("StatefulSet", &set(partial), None)),
            "1 of 3 ready"
        );
        let updating = json!({"observedGeneration": 1, "readyReplicas": 3,
                              "currentRevision": "a", "updateRevision": "b"});
        assert_eq!(
            not_yet(readiness("StatefulSet", &set(updating), None)),
            "update in progress"
        );
    }

    #[test]
    fn daemonset_rules() {
        let set = |status: Json| json!({"metadata": {"generation": 1}, "status": status});
        let ready = json!({"observedGeneration": 1, "desiredNumberScheduled": 2,
                           "numberReady": 2, "updatedNumberScheduled": 2});
        assert_eq!(readiness("DaemonSet", &set(ready), None), Readiness::Ready);
        let partial = json!({"observedGeneration": 1, "desiredNumberScheduled": 2,
                             "numberReady": 1, "updatedNumberScheduled": 2});
        assert_eq!(
            not_yet(readiness("DaemonSet", &set(partial), None)),
            "1 of 2 ready"
        );
    }

    #[test]
    fn job_complete_failed_and_running() {
        let job = |conditions: Json| json!({"status": {"conditions": conditions}});
        let complete = json!([{"type": "Complete", "status": "True"}]);
        assert_eq!(readiness("Job", &job(complete), None), Readiness::Ready);
        let broken =
            json!([{"type": "Failed", "status": "True", "message": "BackoffLimitExceeded"}]);
        assert_eq!(
            failed(readiness("Job", &job(broken), None)),
            "BackoffLimitExceeded"
        );
        assert_eq!(
            not_yet(readiness("Job", &json!({"status": {"active": 1}}), None)),
            "still running"
        );
    }

    #[test]
    fn pod_ready_succeeded_and_failed() {
        let ready = json!({"status": {"phase": "Running",
                                      "conditions": [{"type": "Ready", "status": "True"}]}});
        assert_eq!(readiness("Pod", &ready, None), Readiness::Ready);
        let done = json!({"status": {"phase": "Succeeded"}});
        assert_eq!(readiness("Pod", &done, None), Readiness::Ready);
        let crashed = json!({"status": {"phase": "Failed", "message": "evicted"}});
        assert_eq!(failed(readiness("Pod", &crashed, None)), "evicted");
        let pending = json!({"status": {"phase": "Pending"}});
        assert_eq!(not_yet(readiness("Pod", &pending, None)), "Pending");
    }

    #[test]
    fn custom_condition_on_any_kind() {
        let widget = |status: &str| json!({"status": {"conditions": [{"type": "Available", "status": status}]}});
        assert_eq!(
            readiness("Widget", &widget("True"), Some("Available")),
            Readiness::Ready
        );
        assert_eq!(
            not_yet(readiness("Widget", &widget("False"), Some("Available"))),
            "condition Available is False"
        );
        assert_eq!(
            not_yet(readiness("Widget", &json!({}), Some("Available"))),
            "condition Available is not reported yet"
        );
        // A condition overrides the built-in rule.
        assert_eq!(
            readiness("Deployment", &widget("True"), Some("Available")),
            Readiness::Ready
        );
    }

    #[test]
    fn unknown_kind_without_condition_fails() {
        assert_eq!(
            failed(readiness("Widget", &json!({}), None)),
            "no readiness rule for Widget; give a condition"
        );
    }

    #[test]
    fn default_wait_picks_workloads_only() {
        let id = |kind: &str| ResourceId {
            api_version: "v1".to_owned(),
            kind: kind.to_owned(),
            namespace: Some("shop".to_owned()),
            name: "x".to_owned(),
        };
        let all: Vec<ResourceId> = [
            "ConfigMap",
            "Service",
            "Deployment",
            "Pod",
            "Job",
            "StatefulSet",
            "DaemonSet",
        ]
        .iter()
        .map(|kind| id(kind))
        .collect();
        let kinds: Vec<String> = default_wait(&all).into_iter().map(|id| id.kind).collect();
        assert_eq!(kinds, vec!["Deployment", "Job", "StatefulSet", "DaemonSet"]);
    }

    #[test]
    fn timeout_parsing() {
        assert_eq!(parse_timeout("90s"), Ok(Duration::from_secs(90)));
        assert_eq!(parse_timeout("10m"), Ok(Duration::from_secs(600)));
        assert_eq!(parse_timeout("1h"), Ok(Duration::from_secs(3600)));
        for bad in [
            "10",
            "abc",
            "0s",
            "",
            "-5m",
            "5d",
            "1.5m",
            "99999999999999999999h",
        ] {
            assert!(parse_timeout(bad).is_err(), "{bad:?}");
        }
    }
}
