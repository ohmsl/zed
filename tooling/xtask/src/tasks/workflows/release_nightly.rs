use crate::tasks::workflows::{
    nix_build::build_nix,
    release::{
        ReleaseBundleJobs, create_sentry_release, download_workflow_artifacts, notify_on_failure,
        prep_release_artifacts,
    },
    run_bundling::{bundle_linux, bundle_mac, bundle_windows},
    run_tests::{clippy, run_platform_tests_no_filter},
    runners::{Arch, Platform, ReleaseChannel},
    steps::{
        CommonJobConditions, DEFAULT_REPOSITORY_OWNER_GUARD, FluentBuilder, NamedJob,
        ZippyGitIdentity,
    },
};

use super::{runners, steps, steps::named, vars};
use gh_workflow::*;
use indoc::indoc;

/// Generates the release_nightly.yml workflow
pub fn release_nightly() -> Workflow {
    let check = check_for_changes();
    let style = with_changes_guard(check_style(), &check);
    // Run only on windows as that's our fastest platform right now.
    let tests = with_changes_guard(run_platform_tests_no_filter(Platform::Windows), &check);
    let clippy_job = with_changes_guard(clippy(Platform::Windows, None), &check);
    const NIGHTLY: Option<ReleaseChannel> = Some(ReleaseChannel::Nightly);

    let bundle = ReleaseBundleJobs {
        linux_aarch64: bundle_linux(Arch::AARCH64, NIGHTLY, &[&style, &tests, &clippy_job]),
        linux_x86_64: bundle_linux(Arch::X86_64, NIGHTLY, &[&style, &tests, &clippy_job]),
        mac_aarch64: bundle_mac(Arch::AARCH64, NIGHTLY, &[&style, &tests, &clippy_job]),
        mac_x86_64: bundle_mac(Arch::X86_64, NIGHTLY, &[&style, &tests, &clippy_job]),
        windows_aarch64: bundle_windows(Arch::AARCH64, NIGHTLY, &[&style, &tests, &clippy_job]),
        windows_x86_64: bundle_windows(Arch::X86_64, NIGHTLY, &[&style, &tests, &clippy_job]),
    };

    let nix_linux_x86 = build_nix(
        Platform::Linux,
        Arch::X86_64,
        "default",
        None,
        &[&style, &tests],
    );
    let nix_mac_arm = build_nix(
        Platform::Mac,
        Arch::AARCH64,
        "default",
        None,
        &[&style, &tests],
    );
    let update_nightly_tag = update_nightly_tag_job(&bundle);
    let notify_on_failure = notify_on_failure(&bundle.jobs());

    named::workflow()
        .on(Event::default()
            // Fire hourly
            .schedule([Schedule::new("0 * * * *")])
            .push(Push::default().add_tag("nightly")))
        .concurrency(
            Concurrency::default()
                .group(format!("release-nightly-${{{{ github.event_name }}}}"))
                .cancel_in_progress(true),
        )
        .add_env(("CARGO_TERM_COLOR", "always"))
        .add_env(("RUST_BACKTRACE", "1"))
        .add_job(check.name, check.job)
        .add_job(style.name, style.job)
        .add_job(tests.name, tests.job)
        .add_job(clippy_job.name, clippy_job.job)
        .map(|mut workflow| {
            for job in bundle.into_jobs() {
                workflow = workflow.add_job(job.name, job.job);
            }
            workflow
        })
        .add_job(nix_linux_x86.name, nix_linux_x86.job)
        .add_job(nix_mac_arm.name, nix_mac_arm.job)
        .add_job(update_nightly_tag.name, update_nightly_tag.job)
        .add_job(notify_on_failure.name, notify_on_failure.job)
}

fn check_for_changes() -> NamedJob {
    fn check_nightly_tag() -> (Step<Run>, vars::StepOutput) {
        let step = named::bash(indoc! {r#"
            if [ "$GITHUB_EVENT_NAME" = "push" ]; then
                # Push events always take precedence; cancel any in-progress scheduled runs
                gh run list \
                    --workflow release_nightly.yml \
                    --status in_progress \
                    --event schedule \
                    --json databaseId \
                    -q '.[].databaseId' | while read -r run_id; do
                    echo "Cancelling in-progress scheduled run $run_id"
                    gh run cancel "$run_id" || true
                done
                echo "Push event, proceeding with nightly release"
                echo "bump_nightly=true" >> "$GITHUB_OUTPUT"
                exit 0
            fi

            # For scheduled events: check if a push-triggered run is already in progress
            if gh run list \
                --workflow release_nightly.yml \
                --status in_progress \
                --event push \
                --json databaseId \
                -q '.[].databaseId' | grep -q .; then
                echo "A push-triggered nightly release is in progress, skipping scheduled run"
                echo "bump_nightly=false" >> "$GITHUB_OUTPUT"
                exit 0
            fi

            if ! NIGHTLY_SHA=$(git rev-parse nightly 2>/dev/null); then
                echo "No nightly tag found, changes detected"
                echo "bump_nightly=true" >> "$GITHUB_OUTPUT"
                exit 0
            fi

            HEAD_SHA=$(git rev-parse HEAD)
            if [ "$NIGHTLY_SHA" = "$HEAD_SHA" ]; then
                echo "nightly tag already points to HEAD ($HEAD_SHA), no new nightly needed"
                echo "bump_nightly=false" >> "$GITHUB_OUTPUT"
            else
                echo "Changes detected: nightly=$NIGHTLY_SHA HEAD=$HEAD_SHA"
                echo "bump_nightly=true" >> "$GITHUB_OUTPUT"
            fi
        "#})
        .id("check");

        let bump_nightly = vars::StepOutput::new(&step, "bump_nightly");
        (step, bump_nightly)
    }

    let (check_step, bump_nightly) = check_nightly_tag();

    named::job(
        Job::default()
            .with_repository_owner_guard()
            .permissions(Permissions::default().actions(Level::Write))
            .runs_on(runners::LINUX_SMALL)
            .outputs([("bump_nightly".to_owned(), bump_nightly.to_string())])
            .add_step(steps::checkout_repo().with_fetch_tags())
            .add_step(check_step),
    )
}

fn with_changes_guard(job: NamedJob, check: &NamedJob) -> NamedJob {
    NamedJob {
        name: job.name,
        job: job
            .job
            .add_need(check.name.clone())
            .cond(Expression::new(format!(
                "{DEFAULT_REPOSITORY_OWNER_GUARD} && needs.{}.outputs.bump_nightly == 'true'",
                check.name
            ))),
    }
}

fn check_style() -> NamedJob {
    let job = release_job(&[])
        .runs_on(runners::MAC_DEFAULT)
        .add_step(steps::checkout_repo().with_full_history())
        .add_step(steps::cargo_fmt())
        .add_step(steps::script("./script/clippy"));

    named::job(job)
}

fn release_job(deps: &[&NamedJob]) -> Job {
    let job = Job::default()
        .with_repository_owner_guard()
        .timeout_minutes(60u32);
    if deps.len() > 0 {
        job.needs(deps.iter().map(|j| j.name.clone()).collect::<Vec<_>>())
    } else {
        job
    }
}

fn update_nightly_tag_job(bundle: &ReleaseBundleJobs) -> NamedJob {
    fn update_nightly_tag() -> Step<Run> {
        named::bash(indoc! {r#"
            if [ "$(git rev-parse nightly)" = "$(git rev-parse HEAD)" ]; then
              echo "Nightly tag already points to current commit. Skipping tagging."
              exit 0
            fi
            git tag -f nightly
            git push origin nightly --force
        "#})
        .with_zippy_git_identity()
    }

    NamedJob {
        name: "update_nightly_tag".to_owned(),
        job: steps::release_job(&bundle.jobs())
            .runs_on(runners::LINUX_MEDIUM)
            .add_step(steps::checkout_repo().with_fetch_tags())
            .add_step(download_workflow_artifacts())
            .add_step(steps::script("ls -lR ./artifacts"))
            .add_step(prep_release_artifacts())
            .add_step(
                steps::script("./script/upload-nightly")
                    .add_env((
                        "DIGITALOCEAN_SPACES_ACCESS_KEY",
                        vars::DIGITALOCEAN_SPACES_ACCESS_KEY,
                    ))
                    .add_env((
                        "DIGITALOCEAN_SPACES_SECRET_KEY",
                        vars::DIGITALOCEAN_SPACES_SECRET_KEY,
                    )),
            )
            .add_step(update_nightly_tag())
            .add_step(create_sentry_release()),
    }
}
