import angreal
import subprocess
import os

cwd = os.path.join(angreal.get_root(), '..')


@angreal.command(name="build", about="build the project")
@angreal.argument(name="release", long="release", short="r", takes_value=False, help="build in release mode")
def build(release=False):
    cmd = ["cargo", "build"]
    if release:
        cmd.append("--release")
    result = subprocess.run(cmd, cwd=cwd)
    raise SystemExit(result.returncode)
