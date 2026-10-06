#!/usr/bin/env python3
"""Owner-operated, fail-closed GitHub policy check. Never changes visibility.

Uses the existing `gh` login. API responses and command errors are withheld;
only fixed policy findings are printed. Secret/variable endpoints return counts.
"""
import argparse
import base64
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys

OWNER = "JamieAP"
REPO = "JamieAP/gently"
BASE = f"repos/{REPO}"
CONTEXTS = ["validate (macos-latest)", "validate (ubuntu-latest)"]
FILES = (".github/workflows/ci.yml", ".github/workflows/docs.yml", ".github/CODEOWNERS")
ACTIONS = {"allowed_actions": "selected", "sha_pinning_required": True}
SELECTED = {"github_owned_allowed": True, "verified_allowed": False, "patterns_allowed": []}
WORKFLOW = {"default_workflow_permissions": "read", "can_approve_pull_request_reviews": False}
FORK = {"approval_policy": "all_external_contributors"}
ACTIVE_STATUSES = ("in_progress", "queued", "waiting", "requested", "pending")
CI_CONDITION = "github.repository == 'JamieAP/gently' && github.actor == 'JamieAP' && github.triggering_actor == 'JamieAP' && (github.event_name == 'workflow_dispatch' || (github.event_name == 'push' && github.ref == 'refs/heads/main') || (github.event_name == 'pull_request' && github.event.pull_request.head.repo.full_name == github.repository && github.event.pull_request.user.login == 'JamieAP' && github.event.pull_request.base.ref == 'main'))"
SECURITY_FEATURES = ("secret_scanning", "secret_scanning_push_protection")
PAGES_CONDITION = "github.repository == 'JamieAP/gently' && github.ref == 'refs/heads/main' && github.actor == 'JamieAP' && github.triggering_actor == 'JamieAP'"


def block(text, name, indent):
    """Extract one plain indented mapping from these reviewed workflow files.

    Unsupported syntax fails closed. This is deliberately not a YAML parser.
    """
    lines = text.splitlines()
    start = [i for i, line in enumerate(lines) if line == " " * indent + name + ":"]
    if len(start) != 1:
        raise GuardError("Reviewed workflow must use the expected plain mapping syntax")
    values = []
    for line in lines[start[0] + 1:]:
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        if len(line) - len(line.lstrip()) <= indent:
            break
        values.append(line)
    return "\n".join(values)


class GuardError(Exception):
    def __init__(self, message, status=None):
        super().__init__(message)
        self.status = status


class GitHub:
    def request(self, path, method="GET", body=None, jq=None):
        command = ["gh", "api", "--hostname", "github.com", "--method", method,
                   "-H", "Accept: application/vnd.github+json",
                   "-H", "X-GitHub-Api-Version: 2026-03-10", path]
        if jq:
            command += ["--jq", jq]
        if body is not None:
            command += ["--input", "-"]
        # Keep the existing gh authentication without forwarding collector/vault
        # secrets or enabling gh's API debug logging in this unrelated child.
        env = {name: os.environ[name] for name in (
            "PATH", "HOME", "GH_CONFIG_DIR", "XDG_CONFIG_HOME", "GH_TOKEN", "GITHUB_TOKEN",
            "LANG", "LC_ALL", "HTTPS_PROXY", "HTTP_PROXY", "ALL_PROXY", "NO_PROXY",
            "https_proxy", "http_proxy", "all_proxy", "no_proxy", "SSL_CERT_FILE", "SSL_CERT_DIR",
        ) if name in os.environ}
        env["GH_PROMPT_DISABLED"] = "1"
        try:
            result = subprocess.run(command, input=json.dumps(body) if body is not None else None,
                                    text=True, capture_output=True, timeout=45, check=False, env=env)
        except (OSError, subprocess.TimeoutExpired):
            raise GuardError("GitHub CLI is unavailable or the policy request timed out") from None
        if result.returncode:
            match = re.search(r"\(HTTP (\d{3})\)", result.stderr)
            status = int(match.group(1)) if match else None
            raise GuardError("GitHub rejected a policy request; response withheld", status=status)
        try:
            return json.loads(result.stdout) if result.stdout.strip() else None
        except ValueError:
            raise GuardError("GitHub returned an unexpected policy response; response withheld") from None


class Guard:
    def __init__(self, api, files):
        self.api = api
        self.files = dict(files)

    def get(self, suffix, **kwargs):
        return self.api.request(f"{BASE}/{suffix}", **kwargs)

    def put(self, suffix, body):
        return self.get(suffix, method="PUT", body=body)

    def identity(self):
        user = self.api.request("user")
        repo = self.api.request(BASE)
        if (user.get("login") != OWNER or repo.get("full_name") != REPO
                or repo.get("owner", {}).get("login") != OWNER
                or repo.get("owner", {}).get("type") != "User"
                or repo.get("permissions", {}).get("admin") is not True):
            raise GuardError("Requires JamieAP's authenticated administrator session for JamieAP/gently")
        if repo.get("default_branch") != "main":
            raise GuardError("The default branch must remain main; review the changed repository policy")
        return user, repo

    def inventory(self):
        for status in ACTIVE_STATUSES:
            count = self.get(f"actions/runs?status={status}&per_page=1", jq=".total_count")
            if type(count) is not int or count != 0:
                raise GuardError("Active/queued/waiting workflows must finish or be canceled before applying the public policy")
        collaborators = []
        for page in range(1, 21):
            values = self.get(f"collaborators?affiliation=all&per_page=100&page={page}",
                              jq="map({login})")
            if not isinstance(values, list):
                raise GuardError("Unable to verify collaborator inventory")
            collaborators.extend(values)
            if len(values) < 100:
                break
        else:
            raise GuardError("Collaborator inventory exceeded the bounded audit")
        if any(value.get("login") != OWNER for value in collaborators):
            raise GuardError("Unexpected collaborator: only JamieAP may have repository access")
        for suffix, query, label in (
            ("hooks", "length", "webhooks"), ("keys", "length", "deploy keys"),
            ("invitations", "length", "pending collaborator invitations"),
            ("actions/runners", ".total_count", "self-hosted runners"),
            ("actions/secrets", ".total_count", "repository Actions secrets"),
            ("actions/variables", ".total_count", "repository Actions variables"),
        ):
            count = self.get(suffix, jq=query)
            if type(count) is not int or count != 0:
                raise GuardError(f"Unexpected {label}; review/remove them before enabling public Actions")
        environments = self.get("environments?per_page=100", jq=".environments | map({name})")
        if not isinstance(environments, list) or any(env.get("name") != "github-pages" for env in environments):
            raise GuardError("Unexpected deployment environment; only github-pages is permitted")
        for env in environments:
            for kind in ("secrets", "variables"):
                count = self.get(f"environments/github-pages/{kind}", jq=".total_count")
                if type(count) is not int or count != 0:
                    raise GuardError(f"Unexpected github-pages {kind}; public workflows require no stored credentials")

    def reviewed_workflows(self, reviewed_main_sha=None):
        actual = self.get("git/ref/heads/main").get("object", {}).get("sha")
        if not isinstance(actual, str) or not re.fullmatch(r"[0-9a-f]{40}", actual):
            raise GuardError("Unable to verify main's commit")
        if reviewed_main_sha is not None and actual != reviewed_main_sha:
            raise GuardError("main changed since the owner's reviewed commit; review it again")
        remote_names = self.get(f"contents/.github/workflows?ref={actual}", jq="map(.name)")
        if sorted(remote_names or []) != ["ci.yml", "docs.yml"]:
            raise GuardError("Unexpected workflow inventory on main")
        for name in FILES:
            remote = self.get(f"contents/{name}?ref={actual}")
            try:
                text = base64.b64decode(remote["content"], validate=False).decode("utf-8")
            except (KeyError, ValueError, UnicodeError):
                raise GuardError("Unable to read reviewed workflow policy on main") from None
            if remote.get("type") != "file" or remote.get("encoding") != "base64" or text != self.files.get(name):
                raise GuardError("main does not contain the local owner-reviewed workflow/CODEOWNERS hardening")
        self.validate_files()
        return actual

    def validate_files(self):
        owners = [line.strip() for line in self.files.get(".github/CODEOWNERS", "").splitlines()
                  if line.strip() and not line.lstrip().startswith("#")]
        if owners != ["* @JamieAP"]:
            raise GuardError("CODEOWNERS must assign every file to JamieAP")
        for name in FILES[:2]:
            text = self.files.get(name, "")
            if re.search(r"\b(pull_request_target|workflow_run|issue_comment|issues|discussion_comment|repository_dispatch|self-hosted)\b", text):
                raise GuardError("A reviewed workflow contains a prohibited trigger or self-hosted runner")
            if "secrets." in text or re.search(r"secrets\s*\[", text):
                raise GuardError("Public workflows must not reference stored secrets")
            if block(text, "permissions", 0).strip() != "contents: read":
                raise GuardError("Workflow default permissions must contain only contents: read")
            triggers = re.findall(r"(?m)^  ([A-Za-z0-9_-]+):", block(text, "on", 0))
            expected_triggers = ["pull_request", "push", "workflow_dispatch"] if name.endswith("ci.yml") else ["push", "workflow_dispatch"]
            if sorted(triggers) != expected_triggers:
                raise GuardError("Only guarded owner CI pull requests, main pushes and manual dispatch are permitted")
            jobs = block(text, "jobs", 0)
            expected = ["validate"] if name.endswith("ci.yml") else ["build", "deploy"]
            if re.findall(r"(?m)^  ([A-Za-z0-9_-]+):$", jobs) != expected:
                raise GuardError("Reviewed workflow has an unexpected job inventory")
            for job in expected:
                contents = block(jobs, job, 2)
                conditions = re.findall(r"(?m)^    if: (.+)$", contents)
                if job == "validate":
                    authorize = ["      - name: Authorize owner-operated validation",
                                 "        if: ${{ !(" + CI_CONDITION + ") }}", "        run: exit 1"]
                    steps = block(contents, "steps", 4).splitlines()
                    boundary = next((i for i, line in enumerate(steps[1:], 1) if line.startswith("      - ")), len(steps))
                    if conditions or steps[:boundary] != authorize or "continue-on-error" in contents:
                        raise GuardError("CI must fail an exact owner/repository/author authorization step before checkout")
                elif conditions != [PAGES_CONDITION]:
                    raise GuardError("Every deployment workflow job must use the exact owner/repository/branch guard")
                if job == "deploy":
                    if [line.strip() for line in block(contents, "permissions", 4).splitlines()] != ["pages: write", "id-token: write"]:
                        raise GuardError("Only Pages deployment receives exactly Pages/OIDC write permissions")
                    if block(contents, "environment", 4).splitlines()[0].strip() != "name: github-pages":
                        raise GuardError("Pages deployment must use the protected github-pages environment")
                elif re.search(r"(?m)^    permissions:", contents):
                    raise GuardError("Build/test jobs may not override read-only workflow permissions")
            uses = re.findall(r"(?m)^\s*-?\s*uses:\s*([^\s#]+)", text)
            if not uses or len(uses) != len(re.findall(r"\buses\s*:", text)):
                raise GuardError("Unable to verify action references in the reviewed workflow")
            if any(not re.fullmatch(r"actions/[A-Za-z0-9_-]+@[0-9a-f]{40}", value) for value in uses):
                raise GuardError("Actions must be GitHub-owned and pinned to full commit SHAs")
            if "persist-credentials: false" not in text or "persist-credentials: true" in text:
                raise GuardError("Checkout must not persist repository credentials")
            if name.endswith("ci.yml") and re.search(r":\s*write\b", text):
                raise GuardError("CI must have no write permissions")

    def policy_issues(self, user, expect_enabled=True, check_actions=True):
        issues = []
        repository = self.api.request(BASE)
        security = repository.get("security_and_analysis") or {}
        if any((security.get(feature) or {}).get("status") != "enabled" for feature in SECURITY_FEATURES):
            issues.append("Secret scanning and push protection must be enabled and confirmed by readback")
        if check_actions:
            actions = self.get("actions/permissions")
            if actions.get("enabled") is not expect_enabled or any(actions.get(k) != v for k, v in ACTIONS.items()):
                issues.append("Actions must use selected GitHub-owned actions, full SHA pins, and the expected enabled state")
            if actions.get("allowed_actions") == "selected":
                selected = self.get("actions/permissions/selected-actions")
                if any(selected.get(k) != v for k, v in SELECTED.items()):
                    issues.append("Only GitHub-owned actions may be allowed; verified creators and extra patterns must be disabled")
        workflow = self.get("actions/permissions/workflow")
        if any(workflow.get(k) != v for k, v in WORKFLOW.items()):
            issues.append("Default workflow token must be read-only and unable to approve pull requests")
        if self.get("actions/permissions/fork-pr-contributor-approval").get("approval_policy") != FORK["approval_policy"]:
            issues.append("Every external fork contributor must require Actions approval")
        try:
            protection = self.get("branches/main/protection")
        except GuardError as error:
            if error.status not in (403, 404):
                raise
            protection = {}
        status = protection.get("required_status_checks") or {}
        reviews = protection.get("required_pull_request_reviews") or {}
        if status.get("strict") is not True or sorted(status.get("contexts", [])) != sorted(CONTEXTS):
            issues.append("main must require both Mac and Linux validation checks against the current base")
        if (reviews.get("require_code_owner_reviews") is not True or reviews.get("dismiss_stale_reviews") is not True
                or reviews.get("required_approving_review_count") != 1):
            issues.append("main must require a current code-owner review")
        if any(protection.get(k, {}).get("enabled") is not False for k in ("allow_force_pushes", "allow_deletions")):
            issues.append("main must reject force pushes and deletion")
        if protection.get("enforce_admins", {}).get("enabled") is not False:
            issues.append("Sole-owner policy requires JamieAP's administrator bypass for own pull requests")
        if protection.get("required_conversation_resolution", {}).get("enabled") is not True:
            issues.append("main must require resolved review conversations")
        try:
            env = self.get("environments/github-pages")
        except GuardError as error:
            if error.status != 404:
                raise
            env = None
        if env is None:
            env = {}
        if env.get("can_admins_bypass") is not False:
            issues.append("Disable github-pages administrator bypass in Settings; readback must confirm false")
        if env.get("deployment_branch_policy") != {"protected_branches": False, "custom_branch_policies": True}:
            issues.append("github-pages must use a custom main-only branch policy")
        reviewers = [rule for rule in env.get("protection_rules", []) if rule.get("type") == "required_reviewers"]
        if (len(reviewers) != 1 or reviewers[0].get("prevent_self_review") is not False
                or [(r.get("type"), r.get("reviewer", {}).get("id")) for r in reviewers[0].get("reviewers", [])]
                != [("User", user["id"])]):
            issues.append("github-pages requires JamieAP as its sole deployment reviewer, with self-review allowed")
        policies = self.pages_policies()
        values = policies.get("branch_policies", [])
        if policies.get("total_count") != 1 or [(p.get("name"), p.get("type")) for p in values] != [("main", "branch")]:
            issues.append("github-pages must permit exactly the main branch and no tags")
        return issues

    def pages_policies(self):
        try:
            return self.get("environments/github-pages/deployment-branch-policies")
        except GuardError as error:
            if error.status != 404:
                raise
            return {"total_count": 0, "branch_policies": []}

    def check(self, reviewed_main_sha=None, apps_reviewed=False):
        user, repo = self.identity()
        issues = []
        if repo.get("visibility") == "public" and (not isinstance(reviewed_main_sha, str)
                or not re.fullmatch(r"[0-9a-f]{40}", reviewed_main_sha)):
            issues.append("Public --check requires an explicit owner-reviewed 40-character main commit")
        for operation in (self.inventory, lambda: self.reviewed_workflows(reviewed_main_sha)):
            try:
                operation()
            except GuardError as error:
                issues.append(str(error))
        if not apps_reviewed:
            issues.append("Installed GitHub Apps require an owner review; OAuth cannot enumerate all installations")
        if repo.get("private") is not False or repo.get("visibility") != "public":
            issues.append("Repository is private: public-only branch, fork-approval, and Pages-reviewer controls are not verified; keep Actions disabled during the visibility transition")
        else:
            try:
                issues.extend(self.policy_issues(user))
            except GuardError as error:
                issues.append(str(error))
        return issues

    def disable(self):
        # This safety operation must still work if a stricter policy field is
        # rejected. Policy mutations happen only after disabled readback.
        self.put("actions/permissions", {"enabled": False})
        if self.get("actions/permissions").get("enabled") is not False:
            raise GuardError("GitHub did not confirm that Actions are disabled")

    def branch_update(self):
        """Reject existing controls this fixed baseline could weaken."""
        try:
            current = self.get("branches/main/protection")
        except GuardError as error:
            if error.status != 404:
                raise
            current = {}
        reviews = current.get("required_pull_request_reviews") or {}
        status = current.get("required_status_checks") or {}
        checks = status.get("checks", [])
        extra_contexts = set(status.get("contexts", [])) | {check["context"] for check in checks}
        stronger_flags = ("enforce_admins", "required_linear_history", "required_signatures",
                          "lock_branch", "block_creations", "allow_fork_syncing")
        restricted_reviews = any(
            any((reviews.get(field) or {}).get(kind) for kind in ("users", "teams", "apps"))
            for field in ("dismissal_restrictions", "bypass_pull_request_allowances"))
        known = {"url", "required_status_checks", "required_pull_request_reviews", "restrictions",
                 "allow_force_pushes", "allow_deletions", "required_conversation_resolution", *stronger_flags}
        if (set(current) - known or current.get("restrictions") is not None
                or extra_contexts - set(CONTEXTS) or reviews.get("required_approving_review_count", 0) > 1
                or reviews.get("require_last_push_approval") is True or restricted_reviews
                or any(current.get(key, {}).get("enabled") is True for key in stronger_flags)):
            raise GuardError("Refusing to relax existing branch protection; review its stronger or unexpected controls manually")
        bindings = {check["context"]: check["app_id"] for check in checks if check.get("app_id") is not None}
        if any(type(app_id) is not int for app_id in bindings.values()):
            raise GuardError("Unable to preserve existing branch status-check App bindings")
        required_checks = [{"context": context, **({"app_id": bindings[context]} if context in bindings else {})}
                           for context in CONTEXTS]
        return {
            "required_status_checks": {"strict": True, "checks": required_checks},
            "enforce_admins": False,
            "required_pull_request_reviews": {"dismiss_stale_reviews": True,
                                              "require_code_owner_reviews": True,
                                              "required_approving_review_count": 1},
            "restrictions": None, "allow_force_pushes": False, "allow_deletions": False,
            "required_conversation_resolution": True,
        }, bindings

    def apply(self, reviewed_main_sha, apps_reviewed):
        user, repo = self.identity()
        if repo.get("private") is not False or repo.get("visibility") != "public":
            raise GuardError("--apply is public-only; it never changes repository visibility")
        if not apps_reviewed or not isinstance(reviewed_main_sha, str) or not re.fullmatch(r"[0-9a-f]{40}", reviewed_main_sha):
            raise GuardError("--apply requires --apps-reviewed and an explicit owner-reviewed 40-character main commit")
        try:
            self.disable()
            self.inventory()
            self.reviewed_workflows(reviewed_main_sha)
            branch, bindings = self.branch_update()
            self.api.request(BASE, method="PATCH", body={"security_and_analysis": {
                feature: {"status": "enabled"} for feature in SECURITY_FEATURES}})
            security = (self.api.request(BASE).get("security_and_analysis") or {})
            if any((security.get(feature) or {}).get("status") != "enabled" for feature in SECURITY_FEATURES):
                raise GuardError("GitHub did not confirm secret scanning and push protection")
            self.put("actions/permissions/workflow", WORKFLOW)
            self.put("actions/permissions/fork-pr-contributor-approval", FORK)
            if self.get("actions/permissions/fork-pr-contributor-approval").get("approval_policy") != FORK["approval_policy"]:
                raise GuardError("External-contributor approval must be confirmed before Actions can be enabled")
            self.put("branches/main/protection", branch)
            readback = self.get("branches/main/protection").get("required_status_checks") or {}
            returned = {check["context"]: check.get("app_id") for check in readback.get("checks", [])}
            if any(returned.get(context) != app_id for context, app_id in bindings.items()):
                raise GuardError("GitHub did not preserve existing branch status-check App bindings")
            policies = self.pages_policies()
            values = policies.get("branch_policies", [])
            if (policies.get("total_count") != len(values) or len(values) > 1
                    or any((p.get("name"), p.get("type")) != ("main", "branch") for p in values)):
                raise GuardError("Unexpected github-pages branch policy; remove it manually after review")
            self.put("environments/github-pages", {
                "wait_timer": 0, "prevent_self_review": False,
                "reviewers": [{"type": "User", "id": user["id"]}],
                "deployment_branch_policy": {"protected_branches": False, "custom_branch_policies": True},
            })
            if not values:
                self.get("environments/github-pages/deployment-branch-policies", method="POST",
                         body={"name": "main", "type": "branch"})
            self.inventory()
            self.reviewed_workflows(reviewed_main_sha)
            issues = self.policy_issues(user, expect_enabled=False, check_actions=False)
            if issues:
                raise GuardError("; ".join(issues))
            self.put("actions/permissions", {"enabled": True, **ACTIONS})
            self.put("actions/permissions/selected-actions", SELECTED)
            self.identity()
            self.inventory()
            self.reviewed_workflows(reviewed_main_sha)
            issues = self.policy_issues(user, expect_enabled=True)
            if issues:
                raise GuardError("Policy changed during final verification: " + "; ".join(issues))
        except BaseException as error:
            try:
                self.disable()
            except BaseException:
                raise GuardError("Policy application failed and Actions disabling could not be verified; disable Actions in Settings before proceeding") from None
            if isinstance(error, (GuardError, KeyboardInterrupt)):
                raise GuardError(f"Actions remain disabled. {error or 'Interrupted'}") from None
            raise GuardError("Actions remain disabled. Unexpected policy application failure; response withheld") from None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="Read-only audit (default)")
    mode.add_argument("--plan", action="store_true", help="Print the fixed policy without contacting GitHub")
    mode.add_argument("--apply", action="store_true", help="Apply and verify policy on an already-public repository")
    parser.add_argument("--reviewed-main-sha", help="Owner-reviewed full commit SHA already on remote main")
    parser.add_argument("--apps-reviewed", action="store_true", help="Confirm installed App permissions/events and external automation were manually reviewed")
    args = parser.parse_args()
    if args.plan:
        print("Target: JamieAP/gently, main. No visibility or credential changes.")
        print("Apply disables Actions first, verifies owner-only access and reviewed main workflows, sets secret scanning/push protection, read-only tokens and owner approval gates, verifies owner approval and deployment gates before enabling, then reads back Actions policies.")
        print("Manual prerequisites: review all installed GitHub Apps; disable github-pages administrator bypass; merge and review workflow hardening on main.")
        return 0
    root = Path(__file__).resolve().parent.parent
    def interrupt(signum, frame):
        raise KeyboardInterrupt()
    old_handler = signal.signal(signal.SIGTERM, interrupt)
    try:
        files = {name: (root / name).read_text(encoding="utf-8") for name in FILES}
        guard = Guard(GitHub(), files)
        if args.apply:
            guard.apply(args.reviewed_main_sha, args.apps_reviewed)
            print("Public repository policy verified; Actions enabled. Visibility unchanged.")
        else:
            issues = guard.check(args.reviewed_main_sha, args.apps_reviewed)
            for issue in issues:
                print(f"NOT READY: {issue}")
            if issues:
                return 1
            print("Public repository policy verified (read-only).")
        return 0
    except (GuardError, OSError, UnicodeError) as error:
        print(f"NOT READY: {error if isinstance(error, GuardError) else 'Unable to read local reviewed policy files'}", file=sys.stderr)
        return 1
    finally:
        signal.signal(signal.SIGTERM, old_handler)


if __name__ == "__main__":
    sys.exit(main())
