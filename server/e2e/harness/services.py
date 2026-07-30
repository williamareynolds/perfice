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
    """Compile every service once per session."""
    config.BUILD_DIR.mkdir(parents=True, exist_ok=True)
    binaries: dict[str, Path] = {}
    for spec in config.service_specs():
        out = config.BUILD_DIR / spec.name
        proc = subprocess.run(
            [config.go_bin(), "build", "-o", str(out), spec.package],
            cwd=str(spec.module_dir),
            text=True,
            capture_output=True,
        )
        if proc.returncode != 0:
            raise BuildError(
                f"failed to build {spec.name} in {spec.module_dir}:\n{proc.stderr or proc.stdout}"
            )
        binaries[spec.name] = out
    return binaries


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
