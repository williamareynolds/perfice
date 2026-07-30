"""Build the Go services from local source and supervise them as host processes.

Running them as processes rather than containers keeps the feedback loop short
and, more importantly, gives every test direct access to the service logs when
something fails.
"""

from __future__ import annotations

import os
import shutil
import signal
import subprocess
import time
from pathlib import Path

from . import config
from .infra import InfraError, wait_for_port


class BuildError(RuntimeError):
    pass


def build_all() -> dict[str, Path]:
    """Compile every service once per session, in whichever language it is
    configured to run as."""
    config.BUILD_DIR.mkdir(parents=True, exist_ok=True)
    binaries: dict[str, Path] = {}

    specs = config.service_specs()
    if any(spec.implementation == "rust" for spec in specs):
        # One cargo invocation builds the whole workspace, so doing it per
        # service would just repeat the same work.
        _build_rust_workspace()

    for spec in specs:
        if spec.implementation == "rust":
            binaries[spec.name] = _rust_binary(spec)
        else:
            binaries[spec.name] = _build_go(spec)
    return binaries


def _build_go(spec: config.ServiceSpec) -> Path:
    out = config.BUILD_DIR / f"{spec.name}-go"
    proc = subprocess.run(
        [config.go_bin(), "build", "-o", str(out), spec.go_package],
        cwd=str(spec.go_module_dir),
        text=True,
        capture_output=True,
    )
    if proc.returncode != 0:
        raise BuildError(
            f"failed to build {spec.name} in {spec.go_module_dir}:\n{proc.stderr or proc.stdout}"
        )
    return out


def _build_rust_workspace() -> None:
    """Release, always.

    argon2 is configured for 64 MiB and 3 passes to match the Go
    implementation. Unoptimized, a single password hash takes seconds instead
    of tens of milliseconds, which makes registration-heavy tests look like a
    hang rather than a slow run.
    """
    proc = subprocess.run(
        ["cargo", "build", "--release"],
        cwd=str(config.RUST_DIR),
        text=True,
        capture_output=True,
    )
    if proc.returncode != 0:
        raise BuildError(f"cargo build failed:\n{proc.stderr or proc.stdout}")


def _rust_binary(spec: config.ServiceSpec) -> Path:
    path = config.RUST_DIR / "target" / "release" / spec.cargo_bin
    if not path.exists():
        raise BuildError(
            f"{spec.cargo_bin} was not produced by cargo build; "
            f"is crates/{spec.name} part of the workspace?"
        )
    return path


class ServiceProcess:
    def __init__(self, spec: config.ServiceSpec, binary: Path, log_path: Path):
        self.spec = spec
        self.binary = binary
        self.log_path = log_path
        self.proc: subprocess.Popen | None = None
        self._log_handle = None

    def start(self, ready_timeout: float = 90.0) -> None:
        self.log_path.parent.mkdir(parents=True, exist_ok=True)
        self._log_handle = self.log_path.open("ab")
        env = {**os.environ, **self.spec.env}
        # godotenv/autoload reads a .env from the working directory; run from
        # the build dir so a stray repo .env can never leak into a test run.
        self.proc = subprocess.Popen(
            [str(self.binary)],
            cwd=str(config.BUILD_DIR),
            env=env,
            stdout=self._log_handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            for port in self.spec.ready_ports:
                self._wait_ready(port, ready_timeout)
        except InfraError:
            raise InfraError(
                f"service {self.spec.name} never became ready.\n"
                f"--- last log lines ---\n{self.tail()}"
            ) from None

    def _wait_ready(self, port: int, timeout: float) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.proc is not None and self.proc.poll() is not None:
                raise InfraError(
                    f"service {self.spec.name} exited with code {self.proc.returncode}.\n"
                    f"--- last log lines ---\n{self.tail()}"
                )
            try:
                wait_for_port(port, timeout=1.0)
                return
            except InfraError:
                continue
        raise InfraError(f"{self.spec.name} port {port} never opened")

    def stop(self, timeout: float = 15.0) -> None:
        if self.proc is None:
            return
        if self.proc.poll() is None:
            # start_new_session put the child in its own process group.
            try:
                os.killpg(os.getpgid(self.proc.pid), signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.proc.wait(timeout=timeout)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(os.getpgid(self.proc.pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.proc.wait(timeout=timeout)
        self.proc = None
        if self._log_handle is not None:
            self._log_handle.close()
            self._log_handle = None

    def restart(self) -> None:
        self.stop()
        self.start()

    def tail(self, lines: int = 40) -> str:
        if not self.log_path.exists():
            return "<no log>"
        content = self.log_path.read_text(errors="replace").splitlines()
        return "\n".join(content[-lines:])


class Stack:
    """All four services, started in dependency order."""

    def __init__(self) -> None:
        self.services: dict[str, ServiceProcess] = {}

    def start(self) -> None:
        if config.LOG_DIR.exists():
            shutil.rmtree(config.LOG_DIR)
        config.LOG_DIR.mkdir(parents=True, exist_ok=True)
        binaries = build_all()
        for spec in config.service_specs():
            svc = ServiceProcess(spec, binaries[spec.name], config.LOG_DIR / f"{spec.name}.log")
            svc.start()
            self.services[spec.name] = svc

    def implementations(self) -> dict[str, str]:
        return {spec.name: spec.implementation for spec in config.service_specs()}

    def stop(self) -> None:
        # Reverse order so the gateway stops before what it proxies to.
        for name in reversed(list(self.services)):
            self.services[name].stop()
        self.services.clear()

    def restart(self, name: str) -> None:
        self.services[name].restart()

    def logs(self) -> str:
        return "\n\n".join(
            f"===== {name} =====\n{svc.tail()}" for name, svc in self.services.items()
        )

    def assert_all_running(self) -> None:
        dead = [
            name
            for name, svc in self.services.items()
            if svc.proc is None or svc.proc.poll() is not None
        ]
        if dead:
            raise InfraError(f"services died during the run: {dead}\n\n{self.logs()}")
