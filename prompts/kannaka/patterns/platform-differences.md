# Kannaka Cross-Cutting Pattern: Windows and Linux

Kannaka Labs code runs on Linux servers (Oracle Linux and Debian, systemd,
cron, nginx) and on Windows 11 desktops (PowerShell 5.1, Git Bash, WSL, npm
shims, the kannaka binary at `%USERPROFILE%\.local\bin\kannaka.exe`). Agents on
desktops run production work: swarm seats, model serving, grading. A defect
that only appears on Windows is a real defect.

Report a platform difference only with the concrete line that behaves
differently and the platform where it breaks.

## Sockets and handles

- `TcpStream::try_clone` on Windows duplicates the socket handle; socket
  options set on one handle after the clone (read timeout, non-blocking mode)
  are not visible through the other. kannaka-memory's `Conn` holds a writer and
  a `BufReader` over a clone: a timeout must be set on the handle that reads.
- Options set before the clone are inherited by it.

## Paths

- `~` is expanded by shells only. `KANNAKA_DATA_DIR=~/.kannaka` in a config file
  or service environment reaches the program literally; code must expand it.
- Home directory: `USERPROFILE` on Windows, `HOME` on POSIX; `os.homedir()` /
  `dirs::home_dir()` are safer than either.
- Validators that recognise `/home/<user>` and `/Users/<user>` must also
  recognise `C:\Users\<user>` (and `C:/Users/…`, and `\\`-escaped forms in
  strings).
- `/tmp` does not exist on Windows; use the platform temp directory.

## Line endings and encodings

- Files checked out on Windows have CRLF (`core.autocrlf`). Regexes like
  `^---\n…\n---` and splits on `"\n"` fail on them; use `\r?\n`.
- A shell script with CRLF fails in bash (`$'\r': command not found`).
- PowerShell `>` and `Out-File` write UTF-8 with a BOM by default; strict JSON
  parsers reject it.

## Processes and signals

- npm installs `.cmd`/`.ps1` shims; `spawn("claude")` or `Command::new("node")`
  finds a `.exe` only. Spawning a shim needs a shell or the real executable.
- POSIX signals (`SIGTERM`, `SIGHUP`, `SIGPIPE`) do not exist on Windows;
  shutdown hooks keyed on them never run there.
- `mkdir -m 700` and `chmod` fail or are ignored on NTFS.

## Services

- `systemd-run --scope` has no login `PATH`; a unit's `EnvironmentFile` values
  do not appear in `systemctl show -p Environment`. Cron has a minimal
  environment. Code that works in a terminal can fail under all three.
