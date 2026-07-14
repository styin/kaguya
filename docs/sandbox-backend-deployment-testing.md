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

Install on a Linux host:

```bash
sudo apt-get update
sudo apt-get install -y bubblewrap python3 nodejs bash coreutils
```

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
- Windows Job Objects:
  https://learn.microsoft.com/windows/win32/procthread/job-objects
