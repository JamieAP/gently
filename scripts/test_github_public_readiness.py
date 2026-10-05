"""Public release guard tests; all GitHub responses and mutations are synthetic."""
import base64
import copy
import importlib.util
import pathlib
import re
import subprocess
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location(
    "github_public_readiness", pathlib.Path(__file__).with_name("github-public-readiness.py")
)
guard_module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(guard_module)

OWNER = "JamieAP"
SHA = "a" * 40
FILES = {
    ".github/workflows/ci.yml": """name: code
on:
  pull_request:
  push:
  workflow_dispatch:
permissions:
  contents: read
jobs:
  validate:
    if: github.repository == 'JamieAP/gently' && github.actor == 'JamieAP' && (github.event_name != 'pull_request' || (github.event.pull_request.user.login == 'JamieAP' && github.event.pull_request.head.repo.full_name == 'JamieAP/gently'))
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
        with:
          persist-credentials: false
""",
    ".github/workflows/docs.yml": """name: docs
on:
  push:
  workflow_dispatch:
permissions:
  contents: read
jobs:
  build:
    if: github.repository == 'JamieAP/gently' && github.ref == 'refs/heads/main' && github.actor == 'JamieAP'
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
        with:
          persist-credentials: false
  deploy:
    if: github.repository == 'JamieAP/gently' && github.ref == 'refs/heads/main' && github.actor == 'JamieAP'
    permissions:
      pages: write
      id-token: write
    environment:
      name: github-pages
""",
    ".github/CODEOWNERS": "* @JamieAP\n",
}


class FakeAPI:
    def __init__(self):
        self.calls = []
        self.fail_at = None
        self.user = {"login": OWNER, "id": 7}
        self.repo = {"full_name": "JamieAP/gently", "owner": {"login": OWNER, "type": "User"},
                     "private": False, "visibility": "public", "default_branch": "main",
                     "permissions": {"admin": True}}
        self.collaborators = [{"login": OWNER}]
        self.files = copy.deepcopy(FILES)
        self.main_sha = SHA
        self.counts = {}
        self.actions = {"enabled": True, "allowed_actions": "all", "sha_pinning_required": False}
        self.selected = {"github_owned_allowed": False, "verified_allowed": True, "patterns_allowed": ["*/*"]}
        self.workflow = {"default_workflow_permissions": "write", "can_approve_pull_request_reviews": True}
        self.fork = {"approval_policy": "first_time_contributors"}
        self.protection = None
        self.environment = {"name": "github-pages", "can_admins_bypass": False,
                            "protection_rules": [], "deployment_branch_policy": None}
        self.policies = []

    def request(self, path, method="GET", body=None, jq=None):
        self.calls.append((method, path, copy.deepcopy(body)))
        if self.fail_at and self.fail_at(method, path, body):
            raise guard_module.GuardError("Synthetic API rejection")
        suffix = path.removeprefix("repos/JamieAP/gently/").split("?")[0]
        if path == "user":
            return copy.deepcopy(self.user)
        if path == "repos/JamieAP/gently":
            return copy.deepcopy(self.repo)
        if suffix == "collaborators":
            return copy.deepcopy(self.collaborators) if "page=1" in path else []
        if suffix == "actions/runs":
            status = re.search(r"status=([^&]+)", path).group(1)
            return self.counts.get("actions/runs:" + status, 0)
        if suffix in ("hooks", "keys", "invitations"):
            return self.counts.get(suffix, 0)
        if suffix in ("actions/runners", "actions/secrets", "actions/variables",
                      "environments/github-pages/secrets", "environments/github-pages/variables"):
            return self.counts.get(suffix, 0)
        if suffix == "environments":
            return [{"name": "github-pages"}] if self.environment else []
        if suffix == "git/ref/heads/main":
            return {"object": {"sha": self.main_sha}}
        if suffix == "contents/.github/workflows":
            return ["ci.yml", "docs.yml"]
        if suffix.startswith("contents/"):
            name = suffix.removeprefix("contents/")
            return {"type": "file", "encoding": "base64",
                    "content": base64.b64encode(self.files[name].encode()).decode()}
        if suffix == "actions/permissions":
            if method == "PUT":
                self.actions.update(body)
                return None
            return copy.deepcopy(self.actions)
        if suffix == "actions/permissions/selected-actions":
            if method == "PUT":
                self.selected = copy.deepcopy(body)
                return None
            return copy.deepcopy(self.selected)
        if suffix == "actions/permissions/workflow":
            if method == "PUT":
                self.workflow = copy.deepcopy(body)
                return None
            return copy.deepcopy(self.workflow)
        if suffix == "actions/permissions/fork-pr-contributor-approval":
            if method == "PUT":
                self.fork = copy.deepcopy(body)
                return None
            return copy.deepcopy(self.fork)
        if suffix == "branches/main/protection":
            if method == "PUT":
                self.protection = copy.deepcopy(body)
                self.protection["required_status_checks"]["contexts"] = [
                    check["context"] for check in body["required_status_checks"]["checks"]]
                for key in ("enforce_admins", "allow_force_pushes", "allow_deletions", "required_conversation_resolution"):
                    self.protection[key] = {"enabled": body[key]}
                return copy.deepcopy(self.protection)
            if self.protection is None:
                raise guard_module.GuardError("Synthetic unprotected branch", status=404)
            return copy.deepcopy(self.protection)
        if suffix == "environments/github-pages":
            if method == "PUT":
                if self.environment is None:
                    self.environment = {"name": "github-pages", "can_admins_bypass": True}
                self.environment["deployment_branch_policy"] = copy.deepcopy(body["deployment_branch_policy"])
                self.environment["protection_rules"] = [{
                    "type": "required_reviewers", "prevent_self_review": body["prevent_self_review"],
                    "reviewers": [{"type": "User", "reviewer": {"login": OWNER, "id": 7}}]}]
            return copy.deepcopy(self.environment)
        if suffix == "environments/github-pages/deployment-branch-policies":
            if method == "POST":
                self.policies.append({"id": 1, **body})
                return copy.deepcopy(self.policies[-1])
            return {"total_count": len(self.policies), "branch_policies": copy.deepcopy(self.policies)}
        raise AssertionError(f"Unexpected API call: {method} {path}")


class ReadinessTests(unittest.TestCase):
    def setUp(self):
        self.api = FakeAPI()
        self.guard = guard_module.Guard(self.api, FILES)

    def apply(self):
        self.guard.apply(reviewed_main_sha=SHA, apps_reviewed=True)

    def test_check_never_mutates_and_reports_missing_policy(self):
        issues = self.guard.check(reviewed_main_sha=SHA)
        self.assertTrue(issues)
        self.assertTrue(all(method == "GET" for method, _, _ in self.api.calls))

    def test_private_check_explains_public_only_controls(self):
        self.api.repo.update(private=True, visibility="private")
        self.assertTrue(any("public-only" in issue for issue in self.guard.check(reviewed_main_sha=SHA)))
        self.assertTrue(all(method == "GET" for method, _, _ in self.api.calls))

    def test_private_apply_is_refused_without_mutation(self):
        self.api.repo.update(private=True, visibility="private")
        with self.assertRaisesRegex(guard_module.GuardError, "public"):
            self.apply()
        self.assertTrue(all(method == "GET" for method, _, _ in self.api.calls))

    def test_wrong_owner_or_non_admin_is_refused_without_mutation(self):
        for change in (lambda: self.api.user.update(login="outsider"),
                       lambda: self.api.repo["permissions"].update(admin=False)):
            with self.subTest(change=change):
                self.setUp()
                change()
                with self.assertRaises(guard_module.GuardError):
                    self.apply()
                self.assertTrue(all(method == "GET" for method, _, _ in self.api.calls))

    def test_manual_app_review_and_commit_are_required(self):
        for sha, apps in ((SHA, False), (None, True), ("main", True)):
            with self.subTest(sha=sha, apps=apps):
                with self.assertRaises(guard_module.GuardError):
                    self.guard.apply(reviewed_main_sha=sha, apps_reviewed=apps)
                self.assertTrue(all(method == "GET" for method, _, _ in self.api.calls))

    def test_complete_apply_disables_first_and_enables_only_after_verification(self):
        self.apply()
        writes = [(method, path, body) for method, path, body in self.api.calls if method != "GET"]
        self.assertEqual(writes[0][2]["enabled"], False)
        self.assertEqual(writes[-1][2]["enabled"], True)
        self.assertEqual(self.guard.check(reviewed_main_sha=SHA, apps_reviewed=True), [])

    def test_public_check_requires_explicit_full_reviewed_commit(self):
        self.apply()
        self.api.main_sha = "b" * 40  # Policy files are unchanged; other code is new.
        for sha in (None, "main", "b" * 39):
            with self.subTest(sha=sha):
                issues = self.guard.check(reviewed_main_sha=sha, apps_reviewed=True)
                self.assertTrue(any("40-character" in issue for issue in issues))

    def test_apply_never_relaxes_existing_stronger_branch_controls(self):
        changes = (
            lambda protection: protection["required_status_checks"]["contexts"].append("security-scan"),
            lambda protection: protection.update(restrictions={"users": [{"login": OWNER}], "teams": [], "apps": []}),
            lambda protection: protection["required_pull_request_reviews"].update(required_approving_review_count=2),
            lambda protection: protection["required_pull_request_reviews"].update(require_last_push_approval=True),
            lambda protection: protection["required_pull_request_reviews"].update(dismissal_restrictions={"users": [{"login": OWNER}]}),
            lambda protection: protection.update(required_linear_history={"enabled": True}),
            lambda protection: protection.update(required_signatures={"enabled": True}),
            lambda protection: protection.update(enforce_admins={"enabled": True}),
            lambda protection: protection.update(lock_branch={"enabled": True}),
            lambda protection: protection.update(block_creations={"enabled": True}),
        )
        for change in changes:
            with self.subTest(change=change):
                self.setUp()
                self.apply()
                change(self.api.protection)
                before = copy.deepcopy(self.api.protection)
                self.api.calls.clear()
                with self.assertRaisesRegex(guard_module.GuardError, "existing branch"):
                    self.apply()
                self.assertEqual(self.api.protection, before)
                self.assertFalse(self.api.actions["enabled"])
                self.assertFalse(any(method == "PUT" and path.endswith("branches/main/protection")
                                     for method, path, body in self.api.calls))

    def test_existing_required_check_app_bindings_survive_reapply(self):
        self.apply()
        bindings = [{"context": context, "app_id": 15368} for context in guard_module.CONTEXTS]
        self.api.protection["required_status_checks"]["checks"] = copy.deepcopy(bindings)
        self.apply()
        self.assertEqual(self.api.protection["required_status_checks"]["checks"], bindings)

    def test_inventory_drift_leaves_actions_disabled(self):
        for field in ("hooks", "keys", "invitations", "actions/runners", "actions/secrets", "actions/variables",
                      "environments/github-pages/secrets", "environments/github-pages/variables"):
            with self.subTest(field=field):
                self.setUp()
                self.api.counts[field] = 1
                with self.assertRaises(guard_module.GuardError):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])
        self.setUp()
        self.api.collaborators.append({"login": "outsider"})
        with self.assertRaises(guard_module.GuardError):
            self.apply()
        self.assertFalse(self.api.actions["enabled"])

    def test_all_active_workflow_states_block_apply_and_are_metadata_only(self):
        for status in guard_module.ACTIVE_STATUSES:
            with self.subTest(status=status):
                self.setUp()
                self.api.counts["actions/runs:" + status] = 1
                with self.assertRaisesRegex(guard_module.GuardError, "workflows must finish"):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])
                self.assertFalse(any(method != "GET" and path != "repos/JamieAP/gently/actions/permissions"
                                     for method, path, body in self.api.calls))

    def test_remote_workflow_or_main_drift_leaves_actions_disabled(self):
        for change in (lambda: self.api.files[".github/workflows/ci.yml"].replace("pull_request:", "pull_request_target:"),
                       lambda: "b" * 40):
            with self.subTest(change=change):
                self.setUp()
                if change() == "b" * 40:
                    self.api.main_sha = change()
                else:
                    self.api.files[".github/workflows/ci.yml"] = change()
                with self.assertRaises(guard_module.GuardError):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])

    def test_unsafe_local_reviewed_workflow_is_rejected(self):
        for old, new in (("pull_request:", "pull_request_target:"),
                         ("actions/checkout@" + SHA, "actions/checkout@v4"),
                         ("actions/checkout@" + SHA, "outsider/action@" + SHA),
                         ("ubuntu-latest", "self-hosted"),
                         ("contents: read", "contents: write"),
                         ("persist-credentials: false", "persist-credentials: true")):
            with self.subTest(new=new):
                self.setUp()
                self.guard.files[".github/workflows/ci.yml"] = FILES[".github/workflows/ci.yml"].replace(old, new)
                self.api.files = copy.deepcopy(self.guard.files)
                with self.assertRaises(guard_module.GuardError):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])

    def test_unexpected_pages_branch_policy_is_not_deleted(self):
        self.api.policies = [{"id": 9, "name": "*", "type": "branch"}]
        with self.assertRaisesRegex(guard_module.GuardError, "branch"):
            self.apply()
        self.assertFalse(self.api.actions["enabled"])
        self.assertEqual(self.api.policies[0]["id"], 9)

    def test_pages_admin_bypass_or_missing_field_is_fail_closed(self):
        for present in (True, False):
            with self.subTest(present=present):
                self.setUp()
                if present:
                    self.api.environment["can_admins_bypass"] = True
                else:
                    del self.api.environment["can_admins_bypass"]
                with self.assertRaisesRegex(guard_module.GuardError, "bypass"):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])

    def test_policy_api_failure_or_failed_readback_never_enables(self):
        for fail_at in (
            lambda method, path, body: method == "PUT" and body and "sha_pinning_required" in body,
            lambda method, path, body: method == "PUT" and path.endswith("selected-actions"),
            lambda method, path, body: method == "GET" and path.endswith("fork-pr-contributor-approval")
            and self.api.fork["approval_policy"] == "all_external_contributors",
        ):
            with self.subTest(fail_at=fail_at):
                self.setUp()
                self.api.fail_at = fail_at
                with self.assertRaises(guard_module.GuardError):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])
                self.assertFalse(any(body and body.get("enabled") is True for method, path, body in self.api.calls if method == "PUT"))

    def test_failure_after_enable_disables_again(self):
        enabled_once = False
        def fail_after_enable(method, path, body):
            nonlocal enabled_once
            if method == "PUT" and body and body.get("enabled") is True:
                enabled_once = True
            return enabled_once and method == "GET" and path.endswith("permissions/workflow")
        self.api.fail_at = fail_after_enable
        with self.assertRaises(guard_module.GuardError):
            self.apply()
        self.assertFalse(self.api.actions["enabled"])

    def test_keyboard_interruption_during_apply_keeps_actions_disabled(self):
        def interrupt(method, path, body):
            if method == "PUT" and path.endswith("selected-actions"):
                raise KeyboardInterrupt()
            return False
        self.api.fail_at = interrupt
        with self.assertRaisesRegex(guard_module.GuardError, "Actions remain disabled"):
            self.apply()
        self.assertFalse(self.api.actions["enabled"])

    def test_pages_privilege_and_job_guard_regressions_are_rejected(self):
        pages = FILES[".github/workflows/docs.yml"]
        variants = [
            pages.replace("permissions:\n  contents: read", "permissions:\n  contents: read\n  pages: write\n  id-token: write"),
            pages.replace("  build:\n", "  build:\n    permissions:\n      contents: write\n"),
            pages.replace("      pages: write", "      pages: write\n      contents: write"),
            pages.replace("    if: " + guard_module.PAGES_CONDITION, "    if: always()", 1),
            pages.replace("  deploy:\n    if: " + guard_module.PAGES_CONDITION, "  deploy:\n    if: always()"),
        ]
        for content in variants:
            with self.subTest(content=content):
                self.setUp()
                self.guard.files[".github/workflows/docs.yml"] = content
                self.api.files = copy.deepcopy(self.guard.files)
                with self.assertRaises(guard_module.GuardError):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])

    def test_inventory_identity_or_commit_drift_after_enable_rolls_back(self):
        for kind in ("collaborator", "identity", "commit"):
            with self.subTest(kind=kind):
                self.setUp()
                def drift(method, path, body):
                    if method == "PUT" and body and body.get("enabled") is True:
                        if kind == "collaborator":
                            self.api.collaborators.append({"login": "outsider"})
                        elif kind == "identity":
                            self.api.user["login"] = "outsider"
                        else:
                            self.api.main_sha = "b" * 40
                    return False
                self.api.fail_at = drift
                with self.assertRaises(guard_module.GuardError):
                    self.apply()
                self.assertFalse(self.api.actions["enabled"])

    def test_gh_receives_no_unrelated_secret_or_debug_environment(self):
        result = subprocess.CompletedProcess([], 0, "0\n", "")
        with mock.patch.dict("os.environ", {"GENTLY_TOKEN": "synthetic-collector", "GH_DEBUG": "api"}), \
             mock.patch.object(guard_module.subprocess, "run", return_value=result) as run:
            self.assertEqual(guard_module.GitHub().request("repos/JamieAP/gently/actions/variables", jq=".total_count"), 0)
        self.assertNotIn("GENTLY_TOKEN", run.call_args.kwargs["env"])
        self.assertNotIn("GH_DEBUG", run.call_args.kwargs["env"])
        self.assertEqual(run.call_args.args[0][-2:], ["--jq", ".total_count"])

    def test_gh_error_response_is_withheld(self):
        result = subprocess.CompletedProcess([], 1, "synthetic-private-output", "gh: synthetic-sensitive-response (HTTP 403)")
        with mock.patch.object(guard_module.subprocess, "run", return_value=result):
            with self.assertRaises(guard_module.GuardError) as error:
                guard_module.GitHub().request("user")
        self.assertEqual(error.exception.status, 403)
        self.assertNotIn("synthetic", str(error.exception))


if __name__ == "__main__":
    unittest.main()
