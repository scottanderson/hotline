"""The rule that a commit's area matches the files that it changes."""

import re

from gitlint.options import BoolOption
from gitlint.rules import CommitRule, RuleViolation

# Files that a commit of any area but `plan` and `agents` can also change.
SHARED = {
    "plan.md",
    "AGENTS.md",
    "README.md",
    "HLProtocol.md",
    "Cargo.toml",
    "Cargo.lock",
    "clippy.toml",
    ".gitignore",
    "LICENSE-APACHE",
    "LICENSE-MIT",
}

# Files that the workspace area changes.
WORKSPACE = {"Cargo.toml", "Cargo.lock", "clippy.toml"}


def is_gitlint(path):
    return path == ".gitlint" or path.startswith(".gitlint-rules/")


# The areas that change only their own file.
ONLY = {
    "plan": lambda path: path == "plan.md",
    "agents": lambda path: path == "AGENTS.md",
}

# The areas that own a set of files. A commit of one of them changes at least
# one of its files, and can also change the shared files.
OWN = {
    "gitlint": is_gitlint,
    "workflows": lambda path: path.startswith(".github/workflows/"),
    "readme": lambda path: re.fullmatch(r"(crates/[^/]+/)?README\.md", path) is not None,
    "hlprotocol": lambda path: path == "HLProtocol.md",
}


def crate_of(path):
    """The crate name without `hotline-`, and the path in the crate."""
    match = re.match(r"crates/hotline-([a-z]+)/(.*)", path)
    return match.groups() if match else (None, None)


def module_of(rest):
    """The module path of a file in a crate: `None` for a crate root or a
    file outside `src/`, and `[]` for the crate's README or manifest."""
    if rest in ("README.md", "Cargo.toml"):
        return []
    if not rest.startswith("src/") or not rest.endswith(".rs"):
        return None
    parts = rest[len("src/") : -len(".rs")].split("/")
    if parts[-1] == "mod":
        parts.pop()
    if parts in (["lib"], ["main"]):
        return None
    return parts


def common_module(modules):
    """The longest module path that holds each of `modules`."""
    modules = [module for module in modules if module != []]
    if not modules or None in modules:
        return []
    common = modules[0]
    for module in modules[1:]:
        n = 0
        while n < min(len(common), len(module)) and common[n] == module[n]:
            n += 1
        common = common[:n]
    return common


class AreaMatchesFiles(CommitRule):
    """The title's area names the crate, the module, or the file set that the
    commit changes, as AGENTS.md tells."""

    name = "area-matches-files"
    id = "UC1"
    options_spec = [
        BoolOption(
            "require-module",
            False,
            "Require the module path when the change stays in one module",
        )
    ]

    def validate(self, commit):
        if commit.is_merge_commit:
            return None
        title = commit.message.title
        match = re.match(r"([a-z0-9/_]+): ", title)
        if not match:
            return None
        area = match.group(1)
        files = list(commit.changed_files)
        problem = self.problem(area, files, self.options["require-module"].value)
        if problem:
            return [RuleViolation(self.id, problem, title, 1)]
        return None

    @staticmethod
    def problem(area, files, require_module):
        if area in ONLY:
            others = [f for f in files if not ONLY[area](f)]
            if others:
                return f"Area '{area}' changes only its own file, not {others[0]}"
            return None
        if area in OWN:
            if not any(OWN[area](f) for f in files):
                return f"Area '{area}' changes none of its own files"
            others = [f for f in files if not OWN[area](f) and f not in SHARED]
            if others:
                return f"Area '{area}' cannot change {others[0]}"
            return None
        crates = {}
        for path in files:
            crate, rest = crate_of(path)
            if crate:
                crates.setdefault(crate, []).append(module_of(rest))
            elif path not in SHARED:
                return f"Area '{area}' cannot change {path}"
        if area == "htxf":
            if len(crates) < 2:
                return "Area 'htxf' is for a change to more than one crate"
            return None
        if area == "workspace":
            if len(crates) < 2 and not WORKSPACE & set(files):
                return "Area 'workspace' is for a change to more than one crate or the workspace files"
            return None
        crate, _, module = area.partition("/")
        if crate not in crates:
            return f"Area '{area}' changes no file in crates/hotline-{crate}"
        if len(crates) > 1:
            if module:
                return f"Area '{area}' names a module, but the change is in more than one crate"
            return None
        common = common_module(crates[crate])
        given = module.split("/") if module else []
        if given != common[: len(given)]:
            return f"Area '{area}' names a module that the commit leaves"
        if require_module and common and not given:
            return f"The change stays in one module: use '{crate}/{common[0]}'"
        return None
