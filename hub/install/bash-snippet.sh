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
# Shell integration (OSC 7 cwd + OSC 133 command lifecycle): only inside the
# INNER captured shell hub-relay spawns (HUB_ACTIVE=1 is set there) -- the
# opposite guard from the block above, which only fires in the OUTER,
# uncaptured login shell. bash has no native precmd/preexec, so this uses
# PROMPT_COMMAND + trap DEBUG; guards against the DEBUG trap firing for
# PROMPT_COMMAND's own internals, and captures $? as the FIRST statement in
# the precmd function, before anything else can clobber it.
if [ -n "${HUB_ACTIVE:-}" ] && [ -n "${BASH_VERSION:-}" ]; then
  __hub_cmd_running=0
  __hub_preexec() {
    [ -n "${COMP_LINE:-}" ] && return
    [ "$BASH_COMMAND" = "$PROMPT_COMMAND" ] && return
    if [ "$__hub_cmd_running" = 0 ]; then
      __hub_cmd_running=1
      printf '\033]133;C\007'
    fi
  }
  trap '__hub_preexec' DEBUG
  __hub_precmd() {
    local ec=$?
    if [ "$__hub_cmd_running" = 1 ]; then
      printf '\033]133;D;%s\007' "$ec"
      __hub_cmd_running=0
    fi
    printf '\033]7;file://%s%s\007' "$HOSTNAME" "$PWD"
    printf '\033]133;A\007'
  }
  PROMPT_COMMAND="__hub_precmd${PROMPT_COMMAND:+; $PROMPT_COMMAND}"
fi
# <<< hub shell integration <<<
