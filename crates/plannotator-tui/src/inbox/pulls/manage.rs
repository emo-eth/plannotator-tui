use std::collections::BTreeMap;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Client, query_path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct MergedPull {
    pub(crate) ok: bool,
    pub(crate) key: String,
    pub(crate) method: Option<MergeMethod>,
    pub(crate) url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OwnerAsk {
    Guide,
    Custom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RiskPlace {
    pub(crate) file: String,
    pub(crate) side: String,
    pub(crate) lines: [u64; 2],
    pub(crate) rule: Option<String>,
    pub(crate) why: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RiskJudgment {
    pub(crate) level: RiskLevel,
    pub(crate) why: String,
    #[serde(rename = "where", default)]
    pub(crate) places: Vec<RiskPlace>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RiskAgent {
    pub(crate) id: String,
    pub(crate) label: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PullRisk {
    pub(crate) rules: String,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) agents: Vec<RiskAgent>,
    pub(crate) judgments: BTreeMap<String, RiskJudgment>,
    pub(crate) failed: BTreeMap<String, String>,
    pub(crate) pending: usize,
    pub(crate) last_failure: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RiskModel {
    pub(crate) id: String,
    pub(crate) label: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct RiskModels {
    pub(crate) models: Vec<RiskModel>,
    pub(crate) default_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RiskSettings {
    pub(crate) rules: String,
    pub(crate) provider: Option<String>,
    pub(crate) model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Accepted {
    ok: bool,
}

impl Client {
    pub(crate) fn merge_pull(&self, key: &str, seen_head: &str) -> Result<MergedPull> {
        let response: MergedPull = self
            .pull_post("/api/inbox/pulls/merge", json!({"key":key,"head_sha":seen_head,"confirmed":true}))?;
        ensure!(response.ok, "The Inbox did not confirm the merge.");
        ensure!(
            response.key == key,
            "The Inbox confirmed a different pull request; refresh before continuing."
        );
        Ok(response)
    }

    pub(crate) fn check_pull_owner(&self, keys: &[String], project_id: &str) -> Result<()> {
        let response: Accepted = self.pull_post(
            "/api/inbox/pulls/owner",
            json!({"keys":keys,"project_id":project_id,"validate_only":true}),
        )?;
        ensure!(response.ok, "The Inbox did not accept that project for these pull requests.");
        Ok(())
    }

    pub(crate) fn set_pull_owner(&self, keys: &[String], thread_id: &str, ask: OwnerAsk) -> Result<()> {
        let response: Accepted =
            self.pull_post("/api/inbox/pulls/owner", json!({"keys":keys,"thread_id":thread_id,"ask":ask}))?;
        ensure!(response.ok, "The Inbox did not confirm the ownership change.");
        Ok(())
    }

    pub(crate) fn pull_risk(&self) -> Result<PullRisk> {
        self.pull_get("/api/inbox/pulls/risk")
    }

    pub(crate) fn pull_risk_models(&self, provider: &str) -> Result<RiskModels> {
        self.pull_get(&query_path("/api/inbox/pulls/risk/models", &[("provider", provider)]))
    }

    pub(crate) fn save_pull_risk(&self, settings: &RiskSettings) -> Result<PullRisk> {
        self.pull_post("/api/inbox/pulls/risk", settings)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests assert by panicking")]
mod tests {
    use super::*;

    #[test]
    fn risk_failures_do_not_invent_a_risk_level() {
        let risk: PullRisk = serde_json::from_value(json!({
            "rules":"Require safe migrations", "provider":null, "model":null, "agents":[],
            "judgments":{"one":{"level":"high","why":"Deletes rows","where":[{
                "file":"db.sql","side":"new","lines":[3,4],"rule":"Require safe migrations","why":"Drops records"}]}},
            "failed":{"two":"Provider signed out"},"pending":1,"last_failure":"Provider signed out"
        })).expect("risk response");
        assert_eq!(risk.judgments.get("one").expect("judgment").level, RiskLevel::High);
        assert!(!risk.judgments.contains_key("two"));
        assert_eq!(risk.failed.get("two").map(String::as_str), Some("Provider signed out"));
        assert_eq!(risk.pending, 1);
    }

    #[test]
    fn risk_settings_keep_explicit_null_provider_and_model() {
        let settings = RiskSettings { rules: String::new(), provider: None, model: None };
        assert_eq!(
            serde_json::to_value(settings).expect("settings"),
            json!({"rules":"","provider":null,"model":null})
        );
        assert_eq!(serde_json::to_value(OwnerAsk::Guide).expect("ask"), "guide");
        assert_eq!(serde_json::to_value(OwnerAsk::Custom).expect("ask"), "custom");
    }
}
