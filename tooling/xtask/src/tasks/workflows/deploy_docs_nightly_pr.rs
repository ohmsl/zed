use gh_workflow::{Event, Expression, Input, Job, Level, Permissions, Push, UsesJob, Workflow};

use crate::tasks::workflows::{
    steps::{NamedJob, named},
    vars,
};

const BRANCH_NAME: &str = "staged-docs-releases";

pub(crate) fn deploy_docs_nightly_pr() -> Workflow {
    let deploy_docs = deploy_docs();

    named::workflow()
        .add_event(Event::default().push(Push::default().add_branch(BRANCH_NAME)))
        .add_job(deploy_docs.name, deploy_docs.job)
}

fn deploy_docs() -> NamedJob<UsesJob> {
    let job = Job::default()
        .cond(Expression::new(
            "github.repository_owner == 'zed-industries'",
        ))
        .permissions(Permissions::default().contents(Level::Read))
        .uses_local(".github/workflows/deploy_docs.yml")
        .with(
            Input::default()
                .add("channel", "nightly")
                .add("checkout_ref", "${{ github.sha }}"),
        )
        .secrets(indexmap::IndexMap::from([
            (
                "DOCS_AMPLITUDE_API_KEY".to_owned(),
                vars::DOCS_AMPLITUDE_API_KEY.to_owned(),
            ),
            (
                "CLOUDFLARE_API_TOKEN".to_owned(),
                vars::CLOUDFLARE_API_TOKEN.to_owned(),
            ),
            (
                "CLOUDFLARE_ACCOUNT_ID".to_owned(),
                vars::CLOUDFLARE_ACCOUNT_ID.to_owned(),
            ),
        ]));

    named::job(job)
}
