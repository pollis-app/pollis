#!/usr/bin/env python3
"""The installers' environment variables and their docs agree, both ways (#1721).

Every POLLIS_* variable website/install.sh or website/cli-install.sh reads must be
documented somewhere outside the scripts (docs/, website/, README.md). Every
POLLIS_*=... shown on the same line as an installer command in those docs must be one
that installer actually reads.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = {"install.sh": ROOT / "website/install.sh", "cli-install.sh": ROOT / "website/cli-install.sh"}
READ = re.compile(r"\$\{?(POLLIS_[A-Z_]+)")
SHOWN = re.compile(r"\b(POLLIS_[A-Z_]+)=")


def doc_files():
    out = subprocess.run(["git", "-C", str(ROOT), "ls-files", "docs", "website", "README.md"],
                         check=True, capture_output=True, text=True).stdout.split()
    return [ROOT / p for p in out if p.endswith((".md", ".html")) and not p.endswith(".sh")]


def main():
    reads = {name: set(READ.findall(path.read_text())) for name, path in SCRIPTS.items()}
    if not any(reads.values()):
        sys.exit("check-install-env: no POLLIS_* reads found in the installers; the pattern broke")
    docs = {p: p.read_text(errors="replace") for p in doc_files()}
    corpus = "\n".join(docs.values())
    errors = []
    for name, vars_ in reads.items():
        for var in sorted(vars_):
            if var not in corpus:
                errors.append(f"website/{name} reads {var}, but no doc mentions it (document it in docs/deployments.md)")
    for path, text in docs.items():
        for n, line in enumerate(text.splitlines(), 1):
            for name in SCRIPTS:
                if f"/{name}" not in line and f" {name}" not in line:
                    continue
                for var in SHOWN.findall(line):
                    if var not in reads[name]:
                        errors.append(f"{path.relative_to(ROOT)}:{n}: shows {var}= with {name}, which never reads it")
    if errors:
        print("\n".join(errors))
        sys.exit(1)
    total = sum(len(v) for v in reads.values())
    print(f"Installer env OK — {total} variable(s) read, all documented; no doc shows one an installer ignores.")


if __name__ == "__main__":
    main()
