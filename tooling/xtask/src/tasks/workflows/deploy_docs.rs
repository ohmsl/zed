use gh_workflow::{Event, Expression, Job, Run, Step, Use, Workflow, WorkflowDispatch};

use crate::tasks::workflows::{
    runners,
    steps::{self, FluentBuilder as _, NamedJob, named, release_job},
    vars::{self, StepOutput, WorkflowInput},
};

const BUILD_OUTPUT_DIR: &str = "target/deploy";

pub(crate) enum DocsChannel {
    Nightly,
    Preview,
    Stable,
}

impl DocsChannel {
    pub(crate) fn site_url(&self) -> &'static str {
        match self {
            Self::Nightly => "/docs/nightly/",
            Self::Preview => "/docs/preview/",
            Self::Stable => "/docs/",
        }
    }

    pub(crate) fn project_name(&self) -> &'static str {
        match self {
            Self::Nightly => "docs-nightly",
            Self::Preview => "docs-preview",
            Self::Stable => "docs",
        }
    }

    pub(crate) fn channel_name(&self) -> &'static str {
        match self {
            Self::Nightly => "nightly",
            Self::Preview => "preview",
            Self::Stable => "stable",
        }
    }
}

pub(crate) fn lychee_link_check(dir: &str) -> Step<Use> {
    named::uses(
        "lycheeverse",
        "lychee-action",
        "82202e5e9c2f4ef1a55a3d02563e1cb6041e5332",
    ) // v2.4.1
    .add_with(("args", format!("--no-progress --exclude '^http' '{dir}'")))
    .add_with(("fail", true))
    .add_with(("jobSummary", false))
}

pub(crate) fn install_mdbook() -> Step<Use> {
    named::uses(
        "peaceiris",
        "actions-mdbook",
        "ee69d230fe19748b7abf22df32acaa93833fad08", // v2
    )
    .with(("mdbook-version", "0.4.37"))
}

pub(crate) fn build_docs_book() -> Step<Run> {
    named::bash(indoc::formatdoc! {r#"
        mkdir -p {BUILD_OUTPUT_DIR}
        mdbook build ./docs --dest-dir=../{BUILD_OUTPUT_DIR}/docs/
    "#})
}

fn docs_build_steps(
    job: Job,
    checkout_ref: Option<String>,
    docs_channel: impl Into<String>,
    site_url: impl Into<String>,
) -> Job {
    job.add_env(("DOCS_AMPLITUDE_API_KEY", vars::DOCS_AMPLITUDE_API_KEY))
        .add_step(
            steps::checkout_repo().when_some(checkout_ref, |step, checkout_ref| {
                step.with_ref(checkout_ref)
            }),
        )
        .add_env(("MDBOOK_BOOK__SITE_URL", site_url.into()))
        .add_env(("DOCS_CHANNEL", docs_channel.into()))
        .runs_on(runners::LINUX_XL)
        .add_step(steps::setup_cargo_config(runners::Platform::Linux))
        .add_step(steps::cache_rust_dependencies_namespace())
        .map(steps::install_linux_dependencies)
        .add_step(steps::script("./script/generate-action-metadata"))
        .add_step(lychee_link_check("./docs/src/**/*"))
        .add_step(install_mdbook())
        .add_step(build_docs_book())
        .add_step(lychee_link_check(&format!("{BUILD_OUTPUT_DIR}/docs")))
}

fn docs_deploy_steps(job: Job, project_name: &StepOutput) -> Job {
    fn deploy_to_cf_pages(project_name: &StepOutput) -> Step<Use> {
        named::uses(
            "cloudflare",
            "wrangler-action",
            "da0e0dfe58b7a431659754fdf3f186c529afbe65",
        ) // v3
        .add_with(("apiToken", vars::CLOUDFLARE_API_TOKEN))
        .add_with(("accountId", vars::CLOUDFLARE_ACCOUNT_ID))
        .add_with((
            "command",
            format!(
                "pages deploy {BUILD_OUTPUT_DIR} --project-name=${{{{ {} }}}}",
                project_name.expr()
            ),
        ))
    }

    fn upload_install_script() -> Step<Use> {
        named::uses(
            "cloudflare",
            "wrangler-action",
            "da0e0dfe58b7a431659754fdf3f186c529afbe65",
        ) // v3
        .add_with(("apiToken", vars::CLOUDFLARE_API_TOKEN))
        .add_with(("accountId", vars::CLOUDFLARE_ACCOUNT_ID))
        .add_with((
            "command",
            "r2 object put -f script/install.sh zed-open-source-website-assets/install.sh",
        ))
    }

    fn deploy_docs_worker() -> Step<Use> {
        named::uses(
            "cloudflare",
            "wrangler-action",
            "da0e0dfe58b7a431659754fdf3f186c529afbe65",
        ) // v3
        .add_with(("apiToken", vars::CLOUDFLARE_API_TOKEN))
        .add_with(("accountId", vars::CLOUDFLARE_ACCOUNT_ID))
        .add_with(("command", "deploy .cloudflare/docs-proxy/src/worker.js"))
    }

    fn upload_wrangler_logs() -> Step<Use> {
        named::uses(
            "actions",
            "upload-artifact",
            "ea165f8d65b6e75b540449e92b4886f43607fa02",
        ) // v4
        .if_condition(Expression::new("always()"))
        .add_with(("name", "wrangler_logs"))
        .add_with(("path", "/home/runner/.config/.wrangler/logs/"))
    }

    job.add_step(deploy_to_cf_pages(project_name))
        .add_step(upload_install_script())
        .add_step(deploy_docs_worker())
        .add_step(upload_wrangler_logs())
}

pub(crate) fn check_docs() -> NamedJob {
    NamedJob {
        name: "check_docs".to_owned(),
        job: docs_build_steps(
            release_job(&[]),
            None,
            DocsChannel::Stable.channel_name(),
            DocsChannel::Stable.site_url(),
        ),
    }
}

fn resolve_channel_step(
    channel_expr: impl Into<String>,
) -> (Step<Run>, StepOutput, StepOutput, StepOutput) {
    let step = Step::new("deploy_docs::resolve_channel_step").run(format!(
        indoc::indoc! {r#"
            if [ -z "$CHANNEL" ]; then
                if [ "$GITHUB_REF" = "refs/heads/main" ]; then
                    CHANNEL="nightly"
                else
                    echo "::error::channel input is required when ref is not main."
                    exit 1
                fi
            fi

            case "$CHANNEL" in
                "nightly")
                    SITE_URL="{nightly_site_url}"
                    PROJECT_NAME="{nightly_project_name}"
                    ;;
                "preview")
                    SITE_URL="{preview_site_url}"
                    PROJECT_NAME="{preview_project_name}"
                    ;;
                "stable")
                    SITE_URL="{stable_site_url}"
                    PROJECT_NAME="{stable_project_name}"
                    ;;
                *)
                    echo "::error::Invalid docs channel '$CHANNEL'. Expected one of: nightly, preview, stable."
                    exit 1
                    ;;
            esac

            echo "channel=$CHANNEL" >> "$GITHUB_OUTPUT"
            echo "site_url=$SITE_URL" >> "$GITHUB_OUTPUT"
            echo "project_name=$PROJECT_NAME" >> "$GITHUB_OUTPUT"
        "#},
        nightly_site_url = DocsChannel::Nightly.site_url(),
        preview_site_url = DocsChannel::Preview.site_url(),
        stable_site_url = DocsChannel::Stable.site_url(),
        nightly_project_name = DocsChannel::Nightly.project_name(),
        preview_project_name = DocsChannel::Preview.project_name(),
        stable_project_name = DocsChannel::Stable.project_name(),
    ))
    .id("resolve-channel")
    .add_env(("CHANNEL", channel_expr.into()));

    let channel = StepOutput::new(&step, "channel");
    let site_url = StepOutput::new(&step, "site_url");
    let project_name = StepOutput::new(&step, "project_name");
    (step, channel, site_url, project_name)
}

fn docs_job(channel_expr: impl Into<String>, checkout_ref: Option<String>) -> NamedJob {
    let (resolve_step, channel, site_url, project_name) = resolve_channel_step(channel_expr);

    NamedJob {
        name: "deploy_docs".to_owned(),
        job: docs_deploy_steps(
            docs_build_steps(
                release_job(&[])
                    .name("Build and Deploy Docs")
                    .cond(Expression::new(
                        "github.repository_owner == 'zed-industries'",
                    ))
                    .add_step(resolve_step),
                checkout_ref,
                channel.to_string(),
                site_url.to_string(),
            ),
            &project_name,
        ),
    }
}

pub(crate) fn release_docs_job(
    channel_expr: impl Into<String>,
    checkout_ref: impl Into<String>,
) -> NamedJob {
    docs_job(channel_expr, Some(checkout_ref.into()))
}

pub(crate) fn deploy_docs_job(
    channel_input: &WorkflowInput,
    commit_sha_input: &WorkflowInput,
) -> NamedJob {
    docs_job(
        channel_input.expr(),
        Some(format!(
            "${{{{ {} != '' && {} || github.sha }}}}",
            commit_sha_input.expr(),
            commit_sha_input.expr()
        )),
    )
}

pub(crate) fn deploy_docs() -> Workflow {
    let channel = WorkflowInput::string("channel", Some(String::new()))
        .description("Docs channel to deploy: nightly, preview, or stable");
    let commit_sha = WorkflowInput::string("commit_sha", Some(String::new())).description(
        "Exact commit SHA to checkout and deploy. Defaults to event SHA when omitted.",
    );
    let deploy_docs = deploy_docs_job(&channel, &commit_sha);

    named::workflow()
        .on(Event::default().workflow_dispatch(
            WorkflowDispatch::default()
                .add_input(channel.name, channel.input())
                .add_input(commit_sha.name, commit_sha.input()),
        ))
        .add_job(deploy_docs.name, deploy_docs.job)
}
