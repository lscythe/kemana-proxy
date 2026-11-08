import re
import sys
import tomllib
from pathlib import Path

AREA_SCOPES = {"build", "ci", "deps", "tooling", "docs", "release"}
TYPES_WITHOUT_REQUIRED_SCOPE = {"ci", "docs"}
TYPES = {"feat", "fix", "refactor", "perf", "test", "docs", "build", "ci", "chore", "revert"}
MAX_HEADER_LENGTH = 72
GIT_GENERATED_PREFIXES = ("Merge ", "Revert ", "fixup! ", "squash! ", "amend! ")
HEADER = re.compile(r"^(?P<type>\w+)(?:\((?P<scope>[^)]*)\))?!?: (?P<summary>.*)$")


def allowed_scopes(manifest_text: str) -> set[str]:
    manifest = tomllib.loads(manifest_text)
    scopes = set(AREA_SCOPES)
    package_name = manifest.get("package", {}).get("name")
    if package_name:
        scopes.add(package_name)
    for member in manifest.get("workspace", {}).get("members", []):
        if "*" not in member:
            scopes.add(Path(member).name)
    return scopes


def check_message(message: str, scopes: set[str]) -> str | None:
    if message.startswith(GIT_GENERATED_PREFIXES):
        return None
    header = HEADER.match(message)
    if header is None:
        return f"Commit message must look like 'type(scope): subject', got: {message!r}"
    commit_type = header.group("type")
    if commit_type not in TYPES:
        return f"Unknown commit type '{commit_type}'. Allowed types: {', '.join(sorted(TYPES))}"
    scope = header.group("scope")
    if scope is None:
        if commit_type not in TYPES_WITHOUT_REQUIRED_SCOPE:
            return f"Commit type '{commit_type}' requires a scope. Allowed scopes: {', '.join(sorted(scopes))}"
    elif scope not in scopes:
        return f"Unknown commit scope '{scope}'. Allowed scopes: {', '.join(sorted(scopes))}"
    return check_summary(header.group("summary"), len(message))


def check_summary(summary: str, header_length: int) -> str | None:
    if not summary.strip():
        return "Commit summary must not be empty"
    if not summary[0].islower():
        return f"Commit summary must start with a lowercase letter, got: {summary!r}"
    if summary.endswith("."):
        return "Commit summary must not end with a period"
    if header_length > MAX_HEADER_LENGTH:
        return f"Commit header is {header_length} characters, the limit is {MAX_HEADER_LENGTH}"
    return None


def first_message_line(path: str) -> str:
    for line in Path(path).read_text(encoding="utf-8").splitlines():
        if line.strip() and not line.startswith("#"):
            return line
    return ""


def main(argv: list[str]) -> int:
    manifest = Path(__file__).resolve().parents[2] / "Cargo.toml"
    error = check_message(first_message_line(argv[1]), allowed_scopes(manifest.read_text(encoding="utf-8")))
    if error:
        print(error, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
