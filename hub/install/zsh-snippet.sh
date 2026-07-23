# >>> hub shell integration >>>
# Managed by hub. Do not edit this block. Remove with: hub uninstall
export PATH="$HOME/.hub/bin:$PATH"
if [ -z "${HUB_ACTIVE:-}" ] && [ -z "${HUB_DISABLE:-}" ] && [ -t 1 ] && command -v hub >/dev/null 2>&1; then
  case "$-" in
    # `&& exit`: the relay always exits 0 on a clean session end (you exit the
    # shell, or it's killed from the hub), so close the terminal instead of
    # dropping back to this uncaptured login shell. If `hub attach` fails to
    # start a session it returns NON-zero -> `&& exit` is skipped and this login
    # shell keeps running (fail-safe: a broken hub never bricks the terminal).
    # Non-exec on purpose so we stay a plain child of the login shell.
    *i*) hub attach --new && exit ;;
  esac
fi
# <<< hub shell integration <<<
