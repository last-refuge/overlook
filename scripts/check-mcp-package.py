#!/usr/bin/env python3
"""Exercise the installed stdio entry point using an offline sample profile."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time


TIMEOUT = 20
PROTOCOL = "2026-07-28"
CLIENT = "package-smoke"
READY = b"Overlook MCP sample profile is ready."
INSTALLER = Path(__file__).resolve().parent.parent / "install.sh"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def diagnostics(data, token=""):
    if token:
        data = data.replace(token.encode(), b"[redacted]")
    return data[-2048:].decode(errors="replace").strip()


def run(args, env, *, stdin=b"", token=""):
    result = subprocess.run(args, env=env, input=stdin, capture_output=True, timeout=TIMEOUT)
    require(
        result.returncode == 0,
        f"{args[0]} {list(args[1:3])} exited {result.returncode}. "
        f"Stderr: {diagnostics(result.stderr, token)}",
    )
    return result.stdout


def install_package(archive, destination, env):
    if archive.name.endswith(".tar.gz"):
        run(
            [
                "bash", "-c", 'source "$1"; install_tarball "$2" "$3"',
                "bash", INSTALLER, archive, destination,
            ],
            env,
        )
        return destination / "bin/overlook"

    require(archive.suffix == ".zip", "Expected a Linux .tar.gz or macOS .zip release archive")
    run(["ditto", "-x", "-k", archive, destination], env)
    bundle = destination / "Overlook.app"
    run(["codesign", "--verify", "--strict", bundle], env)
    return bundle / "Contents/MacOS/overlook"


def discover(binary, env, token):
    methods = [
        ("server/discover", {}),
        ("tools/list", {}),
        ("tools/call", {"name": "accounts_list", "arguments": {}}),
    ]
    meta = {
        "io.modelcontextprotocol/protocolVersion": PROTOCOL,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": CLIENT, "version": "1"},
    }
    requests = "".join(
        json.dumps({
            "jsonrpc": "2.0",
            "id": number,
            "method": method,
            "params": {**params, "_meta": meta},
        }) + "\n"
        for number, (method, params) in enumerate(methods, 1)
    )
    output = run(
        [binary, "mcp", "serve", "--client", CLIENT],
        {**env, "OVERLOOK_MCP_TOKEN": token},
        stdin=requests.encode(),
        token=token,
    )
    frames = output.split(b"\n")
    require(frames[-1] == b"" and len(frames) == len(methods) + 1,
            "Expected one newline-terminated JSON-RPC stdout frame per request")
    results = []
    for number, (frame, (method, _)) in enumerate(zip(frames[:-1], methods), 1):
        try:
            reply = json.loads(frame)
        except ValueError:
            raise RuntimeError(f"{method} emitted non-JSON stdout") from None
        require(
            isinstance(reply, dict) and reply.get("jsonrpc") == "2.0"
            and reply.get("id") == number and "error" not in reply
            and isinstance(reply.get("result"), dict)
            and reply["result"].get("resultType") == "complete",
            f"{method} did not return a complete JSON-RPC result",
        )
        results.append(reply["result"])

    discovery, catalog, account_call = results
    require(PROTOCOL in discovery["supportedVersions"] and "tools" in discovery["capabilities"],
            "Discovery did not advertise the expected protocol and tool capability")
    require(any(tool["name"] == "accounts_list" for tool in catalog["tools"]),
            "The discovered tool catalog did not contain accounts_list")
    require(not account_call.get("isError"), "accounts_list failed for the offline sample grant")
    accounts = account_call["structuredContent"]["accounts"]
    require(len(accounts) == 1 and accounts[0]["provider"] == "sample",
            "Expected the one fictional account authorized by the grant")


def qualify(binary, fixture, env):
    # A CLI diagnostic belongs on stderr even before the bridge starts.
    invalid = subprocess.run(
        [binary, "mcp", "serve"], env=env, stdin=subprocess.DEVNULL,
        capture_output=True, timeout=TIMEOUT,
    )
    require(invalid.returncode != 0 and not invalid.stdout and b"--client is required" in invalid.stderr,
            "Invalid MCP serve arguments did not fail with a stderr-only diagnostic")

    owner_stdout = fixture / "owner.stdout"
    owner_stderr = fixture / "owner.stderr"
    with owner_stdout.open("wb") as stdout, owner_stderr.open("wb") as stderr:
        owner = subprocess.Popen(
            [binary, "mcp", "sample", "--offline"], env=env, stdin=subprocess.DEVNULL,
            stdout=stdout, stderr=stderr,
        )

    token = ""
    try:
        deadline = time.monotonic() + TIMEOUT
        while READY not in owner_stderr.read_bytes().splitlines():
            require(owner.poll() is None, f"Sample owner exited early ({owner.returncode})")
            require(time.monotonic() < deadline, "Timed out waiting for sample owner readiness on stderr")
            time.sleep(0.05)

        grant = run([binary, "mcp", "grant", "--client", CLIENT, "--account-id", "sample"], env)
        require(len(grant.splitlines()) == 1 and len(grant.split()) == 1,
                "Sample grant did not issue exactly one credential")
        token = grant.decode("ascii").strip()
        discover(binary, env, token)
        run([binary, "--quit"], env)
        require(owner.wait(timeout=TIMEOUT) == 0, "Sample owner failed to quit cleanly")
        require(owner_stdout.stat().st_size == 0, "Sample owner emitted output on stdout")
    except Exception:
        detail = diagnostics(owner_stderr.read_bytes(), token)
        if detail:
            print(f"Sample owner stderr: {detail}", file=sys.stderr)
        raise
    finally:
        if owner.poll() is None:
            owner.kill()
            owner.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path, help="release tarball or Mac application ZIP")
    archive = parser.parse_args().archive.resolve(strict=True)

    # Use a short absolute path for Unix sockets and spaces in the profile and install paths.
    with tempfile.TemporaryDirectory(prefix="ov mcp ", dir="/tmp") as temp:
        fixture = Path(temp)
        profile = fixture / "sample profile"
        env = {
            "PATH": os.defpath,
            "HOME": str(fixture / "home"),
            "TMPDIR": str(fixture),
            "XDG_CONFIG_HOME": str(fixture / "config"),
            "XDG_DATA_HOME": str(fixture / "data"),
            "XDG_RUNTIME_DIR": str(fixture / "runtime"),
            "OVERLOOK_DATA_DIR": str(profile),
            "OVERLOOK_CONFIG_DIR": str(fixture / "overlook config"),
        }
        for key in ("HOME", "XDG_RUNTIME_DIR"):
            Path(env[key]).mkdir(mode=0o700)
        binary = install_package(archive, fixture / "Overlook install", env)
        qualify(binary, fixture, env)
    print(f"Verified MCP stdio discovery and the offline sample grant in {archive.name}")


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, ValueError, LookupError, TypeError, subprocess.TimeoutExpired) as error:
        sys.exit(f"MCP package smoke test failed: {error}")
