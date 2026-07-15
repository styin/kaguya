# Sandbox backend deployment and contract testing

This document records how to deploy and test Kaguya's Supervisor-owned
`sandbox_exec` backends.

The contract under test is:

- `stdin` is delivered to the executed program.
- execution timeout returns promptly;
- stdout/stderr are capped and report `truncated = true`;
- files persist within one session;
- files do not leak across sessions;
- cleanup followed by reacquisition starts from a clean filesystem.

The shared test suite lives at:

```text
supervisor/tests/sandbox_backend_contract.rs
```

## Backend matrix

| Backend | Host | Isolation meaning | Local test status on 2026-07-14 |
| --- | --- | --- | --- |
| `native` | Windows/macOS/Linux | No host isolation; local process in scratch dir | Passed |
| `docker` | Docker Desktop or Docker Engine | Linux container isolation | Passed on Windows + Docker Desktop + WSL 2 |
| `job_object` | Windows + `--features sandbox-jobobject` | Windows resource/process boundary, not filesystem/network isolation | Passed |
| `bubblewrap` | Linux + `bwrap` | Linux namespace isolation | Not runnable on Windows; gated for Linux CI |

Bubblewrap is Linux-specific. Its Linux CI result does not provide macOS
coverage; macOS is covered by the native Supervisor test matrix, with Docker
Desktop compatibility remaining a separate optional deployment check.

## Automated CI coverage

Pull requests run the Supervisor test suite on Linux, macOS, and Windows, plus
three backend-specific contract jobs:

| CI job | Host | Required backend |
| --- | --- | --- |
| `Sandbox Docker (ubuntu-latest)` | Linux | Docker Engine + `kaguya-sandbox:latest` |
| `Sandbox Bubblewrap (ubuntu-24.04)` | Linux | Bubblewrap + AppArmor profile |
| `Sandbox Job Object (windows-latest)` | Windows | `sandbox-jobobject` feature |

The Docker and Bubblewrap jobs set `KAGUYA_REQUIRE_DOCKER` and
`KAGUYA_REQUIRE_BUBBLEWRAP`, respectively. When set, missing prerequisites fail
the contract instead of taking the permissive local-development skip path.
This ensures a green backend-specific CI job means the backend contract ran.

## Common commands

Run the backend contract suite:

```powershell
cargo test --manifest-path supervisor/Cargo.toml --test sandbox_backend_contract -- --nocapture
```

Run the Windows Job Object contract as well:

```powershell
cargo test --manifest-path supervisor/Cargo.toml --features sandbox-jobobject --test sandbox_backend_contract -- --nocapture
```

Run the broader regression suites:

```powershell
cargo test --manifest-path supervisor/Cargo.toml
cargo test --manifest-path gateway/Cargo.toml
git diff --check
```

## Native backend

Native is the zero-dependency local execution backend. It verifies the full
Gateway -> Supervisor -> Sandbox Provider -> P3 `ToolResult` chain, but it is
not a security sandbox.

No deployment is required.

```powershell
cargo test --manifest-path supervisor/Cargo.toml --test sandbox_backend_contract -- --nocapture
```

Expected contract line:

```text
test native_backend_contract ... ok
```

## Docker backend

Docker is the container-isolated backend. On Windows, Kaguya uses Docker Desktop
with the WSL 2 Linux engine.

### Windows prerequisites

Install/update:

```powershell
winget install --id Docker.DockerDesktop -e --accept-package-agreements --accept-source-agreements
winget install --id Microsoft.WSL -e --accept-package-agreements --accept-source-agreements
wsl --update
```

If Docker Desktop says virtualization is not detected, enable hardware
virtualization in BIOS/UEFI. Docker Desktop also requires the WSL 2 backend on
Windows.

Useful checks:

```powershell
wsl --version
Get-Service com.docker.service
docker context ls
docker info
```

If `docker` is not on the current shell's `PATH`, use Docker Desktop's default
CLI path or reopen the terminal:

```powershell
$env:PATH = 'C:\Program Files\Docker\Docker\resources\bin;' + $env:PATH
docker info
```

If Docker Desktop is installed but the service is stopped, start it from an
administrator PowerShell:

```powershell
Start-Service com.docker.service
```

In this local run, Docker was blocked until:

1. BIOS/UEFI virtualization was enabled;
2. WSL was upgraded from inbox WSL to `Microsoft.WSL`;
3. the WSL 2 kernel package was installed;
4. `com.docker.service` was started with elevation.

### Build the sandbox image

Use the Makefile target:

```powershell
make sandbox-image
```

Or run Docker directly:

```powershell
docker build -t kaguya-sandbox:latest -f docker/sandbox.Dockerfile .
```

Verify the image exists:

```powershell
docker image inspect kaguya-sandbox:latest
```

### Run Docker contract tests

```powershell
cargo test --manifest-path supervisor/Cargo.toml --test sandbox_backend_contract -- --nocapture
```

Expected contract line:

```text
test docker_backend_contract_when_available ... ok
```

The Docker test is environment-gated. It skips instead of failing when Docker is
not installed, the daemon is unavailable, or `kaguya-sandbox:latest` has not
been built.

## Windows Job Object backend

The Job Object backend is feature-gated. It applies Windows memory/process
limits and kill-on-close process cleanup. It does not provide filesystem or
network isolation.

Run:

```powershell
cargo test --manifest-path supervisor/Cargo.toml --features sandbox-jobobject --test sandbox_backend_contract -- --nocapture
```

Expected contract line:

```text
test job_object_backend_contract_when_available ... ok
```

## Bubblewrap backend

Bubblewrap is Linux-only. It is not expected to run on Windows.

Install on an Ubuntu host:

```bash
sudo apt-get update
sudo apt-get install -y bubblewrap python3 nodejs bash coreutils
```

### Ubuntu 24.04 AppArmor prerequisite

Ubuntu 24.04 enables AppArmor mediation for unprivileged user namespaces.
Kaguya runs Bubblewrap with `--unshare-all`, which creates a private network
namespace in addition to the other namespaces. While constructing that
sandbox, Bubblewrap needs namespace-scoped `CAP_NET_ADMIN` to configure its
isolated loopback interface. Without an application-specific AppArmor profile,
that operation can fail with:

```text
bwrap: loopback: Failed RTM_NEWADDR: Operation not permitted
```

Install and load Ubuntu's `bwrap-userns-restrict` profile before running
Kaguya's Bubblewrap backend on Ubuntu 24.04:

```bash
sudo apt-get update
sudo apt-get install -y \
  apparmor-profiles apparmor-utils bubblewrap \
  python3 nodejs bash coreutils
sudo install -m 0644 \
  /usr/share/apparmor/extra-profiles/bwrap-userns-restrict \
  /etc/apparmor.d/bwrap-userns-restrict
sudo apparmor_parser -r /etc/apparmor.d/bwrap-userns-restrict
```

Verify the host configuration and exercise the same isolation mode Kaguya
uses:

```bash
sysctl kernel.apparmor_restrict_unprivileged_userns
dpkg-query -W apparmor apparmor-profiles bubblewrap
sudo aa-status
find /etc/apparmor.d /usr/share/apparmor -iname '*bwrap*'
test "$(cat /proc/sys/kernel/apparmor_restrict_unprivileged_userns)" = "1"
bwrap --ro-bind / / --unshare-all --die-with-parent -- true
```

This profile permits Bubblewrap to perform the privileged namespace setup and
then removes those capabilities from the program executed inside the sandbox.
This prerequisite applies to production Ubuntu 24.04 hosts as well as CI.

Do not work around the failure by setting
`kernel.apparmor_restrict_unprivileged_userns=0`: that relaxes user-namespace
restrictions host-wide for every unconfined process. Do not add `--share-net`
either, because it would give sandboxed, model-authored code access to the host
network and violate Kaguya's offline Bubblewrap contract.

Run:

```bash
cargo test --manifest-path supervisor/Cargo.toml --test sandbox_backend_contract -- --nocapture
```

Expected contract line on Linux with `bwrap` available:

```text
test bubblewrap_backend_contract_when_available ... ok
```

The test is gated with `#[cfg(unix)]` and `bwrap --version`, so Windows and
Linux hosts without Bubblewrap skip it.

## Local verification record

On 2026-07-14, the following commands passed on the Windows development machine
after Docker Desktop and WSL 2 were working:

```powershell
docker build -t kaguya-sandbox:latest -f docker/sandbox.Dockerfile .
cargo test --manifest-path supervisor/Cargo.toml --test sandbox_backend_contract -- --nocapture
cargo test --manifest-path supervisor/Cargo.toml --features sandbox-jobobject --test sandbox_backend_contract -- --nocapture
cargo test --manifest-path supervisor/Cargo.toml
cargo test --manifest-path gateway/Cargo.toml
git diff --check
```

Observed backend contract results:

```text
native_backend_contract ... ok
docker_backend_contract_when_available ... ok
job_object_backend_contract_when_available ... ok
```

## References

- Docker Desktop for Windows installation requirements:
  https://docs.docker.com/desktop/setup/install/windows-install/
- Docker Desktop WSL 2 backend prerequisites:
  https://docs.docker.com/desktop/features/wsl/
- Microsoft WSL installation/update documentation:
  https://learn.microsoft.com/windows/wsl/install
- Bubblewrap manual:
  https://manpages.debian.org/unstable/bubblewrap/bwrap.1.en.html
- Ubuntu 24.04 unprivileged-user-namespace restrictions:
  https://documentation.ubuntu.com/release-notes/24.04/
- Bubblewrap/AppArmor failure and profile discussion:
  https://github.com/openai/codex/issues/14919#issuecomment-4076504751
- Windows Job Objects:
  https://learn.microsoft.com/windows/win32/procthread/job-objects
