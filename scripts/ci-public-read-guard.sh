#!/usr/bin/env bash
set -euo pipefail

die() {
  printf '::error title=Bulkload public-read authority refused::%s\n' "$*" >&2
  exit 1
}

require_equal() {
  local name=$1
  local actual=$2
  local expected=$3
  [[ "$actual" == "$expected" ]] || die "$name drifted"
}

require_empty() {
  local name=$1
  local value=$2
  [[ -z "$value" ]] || die "$name must be empty"
}

require_forbidden_environment_names_absent() {
  local entry name
  while IFS= read -r -d '' entry; do
    name=${entry%%=*}
    case "$name" in
      BASH_FUNC_*%%) die "exported Bash functions are forbidden" ;;
      GIT_*) die "Git environment overrides are forbidden" ;;
      GRPC_PROXY_EXP) die "gRPC proxy override must be absent" ;;
      JUST_*) die "Just environment overrides are forbidden" ;;
      NIX_MIRRORS_*) die "dynamic Nix mirror overrides are forbidden" ;;
      SHELLCHECK_OPTS) die "ShellCheck environment overrides are forbidden" ;;
    esac
  done < <(/usr/bin/env -0)
}

require_endpoint() {
  local name=$1
  local value=$2
  local authority_pattern='([A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?|\[[0-9A-Fa-f:]+\])(:[0-9]{1,5})?/?'

  [[ "$value" != *..* ]] || die "$name contains an invalid authority"
  case "$name" in
    ATTIC_SERVER)
      [[ "$value" =~ ^(http|https)://${authority_pattern}$ ]] ||
        die "$name must be an injected HTTP(S) authority-only endpoint"
      ;;
    BAZEL_REMOTE_CACHE)
      [[ "$value" =~ ^(http|https|grpc|grpcs)://${authority_pattern}$ ]] ||
        die "$name must be an injected HTTP(S) or gRPC authority-only endpoint"
      ;;
    *) die "unknown endpoint contract" ;;
  esac
}

require_github_env() {
  [[ -n "${GITHUB_ENV:-}" ]] || die "GitHub environment file is unavailable"
  [[ -n "${RUNNER_TEMP:-}" ]] || die "runner temporary directory is unavailable"
  [[ -f "$GITHUB_ENV" && ! -L "$GITHUB_ENV" && -O "$GITHUB_ENV" ]] ||
    die "GitHub environment file is not a regular owner-controlled file"

  local env_dir runner_temp
  env_dir=$(cd -P -- "$(dirname -- "$GITHUB_ENV")" && pwd)
  runner_temp=$(cd -P -- "$RUNNER_TEMP" && pwd)
  require_equal "GitHub environment directory" "$env_dir" "$runner_temp/_runner_file_commands"
  case "$(basename -- "$GITHUB_ENV")" in
    set_env_*) ;;
    *) die "GitHub environment file name is outside runner authority" ;;
  esac
}

require_github_output() {
  [[ -n "${GITHUB_OUTPUT:-}" ]] || die "GitHub output file is unavailable"
  [[ -n "${RUNNER_TEMP:-}" ]] || die "runner temporary directory is unavailable"
  [[ -f "$GITHUB_OUTPUT" && ! -L "$GITHUB_OUTPUT" && -O "$GITHUB_OUTPUT" ]] ||
    die "GitHub output file is not a regular owner-controlled file"

  local output_dir runner_temp
  output_dir=$(cd -P -- "$(dirname -- "$GITHUB_OUTPUT")" && pwd)
  runner_temp=$(cd -P -- "$RUNNER_TEMP" && pwd)
  require_equal "GitHub output directory" "$output_dir" "$runner_temp/_runner_file_commands"
  case "$(basename -- "$GITHUB_OUTPUT")" in
    set_output_*) ;;
    *) die "GitHub output file name is outside runner authority" ;;
  esac
}

require_absent_or_empty_file() {
  local name=$1
  local path=$2
  if [[ -e "$path" || -L "$path" ]]; then
    [[ -f "$path" && ! -L "$path" && ! -s "$path" ]] ||
      die "$name must be absent or an empty regular file"
  fi
}

require_private_directory() {
  local name=$1
  local path=$2
  local mode
  [[ -d "$path" && ! -L "$path" && -O "$path" ]] ||
    die "$name must be an owner-controlled directory"
  if mode=$(stat -f '%Lp' "$path" 2>/dev/null); then
    :
  else
    mode=$(stat -c '%a' "$path")
  fi
  require_equal "$name mode" "$mode" 700
}

require_file_sha256() {
  local name=$1
  local path=$2
  local expected=$3
  [[ -f "$path" && ! -L "$path" ]] || die "$name is unavailable"
  require_equal "$name digest" "$(sha256sum "$path" | awk '{print $1}')" "$expected"
}

readonly ci_templates_rev=139bd4c7deabbe07c918dc764a3b9f054066431d
readonly public_key='main:eaUydxuDu7xBoy5cCo3MdknYAkVyTIASQ7DGuwxa+XA='
readonly nixos_cache='https://cache.nixos.org/'
readonly nixos_public_key='cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY='
readonly public_site=bulkload-ci
readonly cache_name=main
readonly reviewed_path=/nix/var/nix/profiles/default/bin:/usr/bin:/bin:/usr/sbin:/sbin
readonly reviewed_step_path=/usr/bin:/bin:/usr/sbin:/sbin
readonly preflight_nix_config="substituters = ${nixos_cache}
store = local
allow-symlinked-store = false
trusted-public-keys = ${nixos_public_key}
trusted-substituters =
builders =
builders-use-substitutes = false
build-hook =
pre-build-hook =
post-build-hook =
diff-hook =
run-diff-hook = false
require-sigs = true
access-tokens =
netrc-file = /dev/null
accept-flake-config = false
secret-key-files =
plugin-files ="
readonly workspace_bazelrc_sha256=15aa8306cc530bbc4d143dd7a6a2f0cfd3efbed19c01503d35bbec0c5e7cd357
readonly flywheel_bazelrc_sha256=f5a7f5116ce0a69471e71b44666fc868e361ed540a40c28a4ee8adc344c87592
readonly bazel_version_sha256=4fa9948d0ae7007cbd1cc05768bc3e7cc6ec46ad0ea84c87df79e7a0c48d76b4
readonly flake_sha256=d3bafa87bfc6675db39cf3567ebc7e740ce4fb842d5353d7d2c51137961268c1
readonly flake_lock_sha256=ccd790af791b173623983382a78bd9476760b9fa9e9e617108e2ae3d1040d19d

mode=${1:-}
case "$mode" in
  preflight | enforce | bazel) ;;
  *) die "mode must be preflight, enforce, or bazel" ;;
esac

require_forbidden_environment_names_absent
require_equal "command search path" "${PATH:-}" "$reviewed_path"
require_equal "private runtime home" "${HOME:-}" "${BULKLOAD_RUNTIME_HOME:-}"
require_empty "user-name override" "${USER:-}"
require_empty "Windows user-name override" "${USERNAME:-}"
require_empty "login-name override" "${LOGNAME:-}"
require_empty "LD audit injection" "${LD_AUDIT:-}"
require_empty "LD library path" "${LD_LIBRARY_PATH:-}"
require_empty "LD preload" "${LD_PRELOAD:-}"
require_empty "Darwin fallback library path" "${DYLD_FALLBACK_LIBRARY_PATH:-}"
require_empty "Darwin framework path" "${DYLD_FRAMEWORK_PATH:-}"
require_empty "Darwin inserted libraries" "${DYLD_INSERT_LIBRARIES:-}"
require_empty "Darwin library path" "${DYLD_LIBRARY_PATH:-}"
require_empty "Nix store directory override" "${NIX_STORE_DIR:-}"
require_empty "legacy Nix store override" "${NIX_STORE:-}"
require_empty "Nix state directory override" "${NIX_STATE_DIR:-}"
require_empty "Nix data directory override" "${NIX_DATA_DIR:-}"
require_empty "Nix log directory override" "${NIX_LOG_DIR:-}"
require_empty "Nix configuration directory override" "${NIX_CONF_DIR:-}"
require_empty "Nix daemon socket override" "${NIX_DAEMON_SOCKET_PATH:-}"
require_empty "Nix symlink-store override" "${NIX_IGNORE_SYMLINK_STORE:-}"
require_empty "Nix libexec directory override" "${NIX_LIBEXEC_DIR:-}"
require_empty "Nix binary directory override" "${NIX_BIN_DIR:-}"
require_empty "Nix remote systems override" "${NIX_REMOTE_SYSTEMS:-}"
require_empty "Nix TLS certificate override" "${NIX_SSL_CERT_FILE:-}"
require_empty "generic TLS certificate override" "${SSL_CERT_FILE:-}"
require_empty "generic TLS certificate directory override" "${SSL_CERT_DIR:-}"
require_empty "curl CA bundle override" "${CURL_CA_BUNDLE:-}"
require_empty "TLS key log" "${SSLKEYLOGFILE:-}"
require_empty "Nix curl flags" "${NIX_CURL_FLAGS:-}"
require_empty "Nix hashed mirrors" "${NIX_HASHED_MIRRORS:-}"
require_empty "HTTP proxy" "${http_proxy:-}"
require_empty "HTTPS proxy" "${https_proxy:-}"
require_empty "FTP proxy" "${ftp_proxy:-}"
require_empty "all-protocol proxy" "${all_proxy:-}"
require_empty "proxy bypass" "${no_proxy:-}"
require_empty "uppercase HTTP proxy" "${HTTP_PROXY:-}"
require_empty "uppercase HTTPS proxy" "${HTTPS_PROXY:-}"
require_empty "uppercase FTP proxy" "${FTP_PROXY:-}"
require_empty "uppercase all-protocol proxy" "${ALL_PROXY:-}"
require_empty "uppercase proxy bypass" "${NO_PROXY:-}"
require_empty "Java tool options" "${JAVA_TOOL_OPTIONS:-}"
require_empty "JDK Java options" "${JDK_JAVA_OPTIONS:-}"
require_empty "legacy Java options" "${_JAVA_OPTIONS:-}"
require_empty "Java home override" "${JAVA_HOME:-}"
require_empty "Java command override" "${JAVACMD:-}"
require_empty "Bazel shell override" "${BAZEL_SH:-}"
require_empty "Bazelisk no-JDK selector" "${BAZELISK_NOJDK:-}"
require_empty "Bazelisk clean command" "${BAZELISK_CLEAN:-}"
require_empty "Bazelisk shutdown command" "${BAZELISK_SHUTDOWN:-}"
require_empty "Bazelisk fallback version" "${USE_BAZEL_FALLBACK_VERSION:-}"
require_empty "Bazel test temporary root" "${TEST_TMPDIR:-}"
require_private_directory "Nix/XDG runtime home" "${BULKLOAD_RUNTIME_HOME:-}"
for runtime_home_name in \
  NIX_CACHE_HOME \
  NIX_CONFIG_HOME \
  NIX_DATA_HOME \
  NIX_STATE_HOME \
  XDG_CACHE_HOME \
  XDG_CONFIG_HOME \
  XDG_DATA_HOME \
  XDG_STATE_HOME; do
  require_equal \
    "$runtime_home_name" \
    "${!runtime_home_name:-}" \
    "$BULKLOAD_RUNTIME_HOME"
done

require_equal "runner environment" "${BULKLOAD_RUNNER_ENVIRONMENT:-}" self-hosted
[[ "${BULKLOAD_RUNNER_NAME:-}" =~ ^bulkload-nix-[a-z0-9]+-runner-[a-z0-9]+$ ]] ||
  die "runner name is outside the bulkload-nix ARC scale set"
require_equal "repository" "${BULKLOAD_REPOSITORY:-}" "${GITHUB_REPOSITORY:-}"
require_equal "event" "${BULKLOAD_EVENT_NAME:-}" "${GITHUB_EVENT_NAME:-}"
require_equal "ref" "${BULKLOAD_REF:-}" "${GITHUB_REF:-}"
case "${BULKLOAD_EVENT_NAME:-}" in
  pull_request)
    require_equal \
      "pull-request head repository" \
      "${BULKLOAD_HEAD_REPOSITORY:-}" \
      "${BULKLOAD_REPOSITORY:-}"
    ;;
  push)
    require_equal \
      "push repository" \
      "${BULKLOAD_HEAD_REPOSITORY:-}" \
      "${BULKLOAD_REPOSITORY:-}"
    ;;
  *) die "event is outside the reviewed push/pull_request inventory" ;;
esac

[[ "${BULKLOAD_EXPECTED_SHA:-}" =~ ^[0-9a-f]{40}$ ]] || die "expected SHA is not canonical"
require_equal \
  "checked-out revision" \
  "$(git -C "${GITHUB_WORKSPACE:?}" rev-parse HEAD)" \
  "$BULKLOAD_EXPECTED_SHA"

expected_upload=false
if [[ "$BULKLOAD_EVENT_NAME" == push && "$BULKLOAD_REF" == refs/heads/main ]]; then
  expected_upload=true
fi
require_equal \
  "Bazel upload gate" \
  "${BULKLOAD_UPLOAD_BAZEL_RESULTS:-}" \
  "$expected_upload"

require_endpoint ATTIC_SERVER "${ATTIC_SERVER:-}"
require_endpoint BAZEL_REMOTE_CACHE "${BAZEL_REMOTE_CACHE:-}"
readonly nix_config="substituters = ${ATTIC_SERVER%/}/${cache_name} ${nixos_cache}
store = local
allow-symlinked-store = false
trusted-public-keys = ${public_key} ${nixos_public_key}
trusted-substituters =
builders =
builders-use-substitutes = false
build-hook =
pre-build-hook =
post-build-hook =
diff-hook =
run-diff-hook = false
require-sigs = true
access-tokens =
netrc-file = /dev/null
accept-flake-config = false
secret-key-files =
plugin-files ="
require_empty "Attic token" "${ATTIC_TOKEN:-}"
require_empty "Nix access tokens" "${NIX_ACCESS_TOKENS:-}"
require_equal "Nix user configuration" "${NIX_USER_CONF_FILES:-}" /dev/null
require_equal "netrc environment" "${NETRC:-}" /dev/null
require_equal "Nix remote store" "${NIX_REMOTE:-}" local
require_empty "remote executor" "${BAZEL_REMOTE_EXECUTOR:-}"
require_empty "remote execution header" "${BAZEL_REMOTE_EXEC_HEADER:-}"
require_empty "Bazel credential helper" "${BAZEL_CREDENTIAL_HELPER:-}"
require_empty "Bazel remote header" "${BAZEL_REMOTE_HEADER:-}"
require_empty "Bazel cache header" "${BAZEL_REMOTE_CACHE_HEADER:-}"
require_empty "Bazelisk GitHub token" "${BAZELISK_GITHUB_TOKEN:-}"
require_empty "Bazelisk base URL override" "${BAZELISK_BASE_URL:-}"
require_empty "Bazelisk format URL override" "${BAZELISK_FORMAT_URL:-}"
require_empty "Bazelisk wrapper directory" "${BAZELISK_WRAPPER_DIRECTORY:-}"
require_empty "Bazelisk incompatible flags" "${BAZELISK_INCOMPATIBLE_FLAGS:-}"
require_empty "Bazelisk verification override" "${BAZELISK_VERIFY_SHA256:-}"
require_empty "Bazelisk version override" "${USE_BAZEL_VERSION:-}"
require_empty "Bazelisk Linux home override" "${BAZELISK_HOME_LINUX:-}"
require_empty "Bazelisk Darwin home override" "${BAZELISK_HOME_DARWIN:-}"
require_equal "Bazelisk wrapper skip" "${BAZELISK_SKIP_WRAPPER:-}" true
require_empty "Bazelisk home" "${BAZELISK_HOME:-}"
require_github_env

if [[ "$mode" != bazel ]]; then
  require_equal "pre-discovery Nix client configuration" "${NIX_CONFIG:-}" "$preflight_nix_config"
fi

emit_common_environment() {
  printf 'HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'USER=\n'
  printf 'USERNAME=\n'
  printf 'LOGNAME=\n'
  printf 'BASH_ENV=/dev/null\n'
  printf 'ENV=/dev/null\n'
  printf 'LD_AUDIT=\n'
  printf 'LD_LIBRARY_PATH=\n'
  printf 'LD_PRELOAD=\n'
  printf 'DYLD_FALLBACK_LIBRARY_PATH=\n'
  printf 'DYLD_FRAMEWORK_PATH=\n'
  printf 'DYLD_INSERT_LIBRARIES=\n'
  printf 'DYLD_LIBRARY_PATH=\n'
  printf 'NIX_STORE_DIR=\n'
  printf 'NIX_STORE=\n'
  printf 'NIX_STATE_DIR=\n'
  printf 'NIX_DATA_DIR=\n'
  printf 'NIX_LOG_DIR=\n'
  printf 'NIX_CONF_DIR=\n'
  printf 'NIX_DAEMON_SOCKET_PATH=\n'
  printf 'NIX_IGNORE_SYMLINK_STORE=\n'
  printf 'NIX_LIBEXEC_DIR=\n'
  printf 'NIX_BIN_DIR=\n'
  printf 'NIX_REMOTE_SYSTEMS=\n'
  printf 'NIX_SSL_CERT_FILE=\n'
  printf 'SSL_CERT_FILE=\n'
  printf 'SSL_CERT_DIR=\n'
  printf 'CURL_CA_BUNDLE=\n'
  printf 'SSLKEYLOGFILE=\n'
  printf 'NIX_CURL_FLAGS=\n'
  printf 'NIX_HASHED_MIRRORS=\n'
  printf 'http_proxy=\n'
  printf 'https_proxy=\n'
  printf 'ftp_proxy=\n'
  printf 'all_proxy=\n'
  printf 'no_proxy=\n'
  printf 'HTTP_PROXY=\n'
  printf 'HTTPS_PROXY=\n'
  printf 'FTP_PROXY=\n'
  printf 'ALL_PROXY=\n'
  printf 'NO_PROXY=\n'
  printf 'SHELLOPTS=\n'
  printf 'BASHOPTS=\n'
  printf 'PS4=\n'
  printf 'BASH_XTRACEFD=\n'
  printf 'JAVA_TOOL_OPTIONS=\n'
  printf 'JDK_JAVA_OPTIONS=\n'
  printf '_JAVA_OPTIONS=\n'
  printf 'JAVA_HOME=\n'
  printf 'JAVACMD=\n'
  printf 'BAZEL_SH=\n'
  printf 'BAZELISK_NOJDK=\n'
  printf 'BAZELISK_CLEAN=\n'
  printf 'BAZELISK_SHUTDOWN=\n'
  printf 'USE_BAZEL_FALLBACK_VERSION=\n'
  printf 'TEST_TMPDIR=\n'
  printf 'NIX_CACHE_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'NIX_CONFIG_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'NIX_DATA_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'NIX_STATE_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'XDG_CACHE_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'XDG_CONFIG_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'XDG_DATA_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'XDG_STATE_HOME=%s\n' "$BULKLOAD_RUNTIME_HOME"
  printf 'ATTIC_TOKEN=\n'
  printf 'NIX_ACCESS_TOKENS=\n'
  printf 'NIX_USER_CONF_FILES=/dev/null\n'
  printf 'NETRC=/dev/null\n'
  printf 'NIX_REMOTE=local\n'
  printf 'BAZEL_CREDENTIAL_HELPER=\n'
  printf 'BAZEL_REMOTE_CACHE_HEADER=\n'
  printf 'BAZEL_REMOTE_EXECUTOR=\n'
  printf 'BAZEL_REMOTE_EXEC_HEADER=\n'
  printf 'BAZEL_REMOTE_HEADER=\n'
  printf 'BAZELISK_BASE_URL=\n'
  printf 'BAZELISK_FORMAT_URL=\n'
  printf 'BAZELISK_GITHUB_TOKEN=\n'
  printf 'BAZELISK_HOME=\n'
  printf 'BAZELISK_HOME_DARWIN=\n'
  printf 'BAZELISK_HOME_LINUX=\n'
  printf 'BAZELISK_INCOMPATIBLE_FLAGS=\n'
  printf 'BAZELISK_VERIFY_SHA256=\n'
  printf 'BAZELISK_SKIP_WRAPPER=true\n'
  printf 'BAZELISK_WRAPPER_DIRECTORY=\n'
  printf 'USE_BAZEL_VERSION=\n'
}

if [[ "$mode" == preflight ]]; then
  {
    emit_common_environment
    printf 'NIX_CONFIG<<BULKLOAD_PREFLIGHT_NIX_CONFIG_%s\n' "$ci_templates_rev"
    printf '%s\n' "$preflight_nix_config"
    printf 'BULKLOAD_PREFLIGHT_NIX_CONFIG_%s\n' "$ci_templates_rev"
  } >> "$GITHUB_ENV"
  exit 0
fi

if [[ "$mode" == bazel ]]; then
  case "${BULKLOAD_BAZEL_PHASE:-}" in
    build | test) ;;
    *) die "Bazel phase must be build or test" ;;
  esac
  require_equal \
    "captured Bazel endpoint" \
    "${BAZEL_REMOTE_CACHE:-}" \
    "${BULKLOAD_CAPTURED_BAZEL_REMOTE_CACHE:-}"
  require_equal \
    "captured Bazel upload gate" \
    "$expected_upload" \
    "${BULKLOAD_CAPTURED_BAZEL_UPLOAD:-}"
  require_equal \
    "active Bazel upload gate" \
    "${GF_BAZEL_REMOTE_UPLOAD:-}" \
    "${BULKLOAD_CAPTURED_BAZEL_UPLOAD:-}"
  require_equal \
    "captured Nix client configuration" \
    "${NIX_CONFIG:-}" \
    "${BULKLOAD_CAPTURED_NIX_CONFIG:-}"
  require_equal "reviewed Nix client configuration" "${NIX_CONFIG:-}" "$nix_config"
  require_absent_or_empty_file "system Bazel rc" /etc/bazel.bazelrc
  [[ ! -e "$GITHUB_WORKSPACE/.bazeliskrc" && ! -L "$GITHUB_WORKSPACE/.bazeliskrc" ]] ||
    die "workspace Bazelisk rc is forbidden"
  [[ ! -f "$GITHUB_WORKSPACE/tools/bazel" && ! -L "$GITHUB_WORKSPACE/tools/bazel" ]] ||
    die "workspace Bazelisk wrapper is forbidden"
  require_file_sha256 \
    "workspace Bazel rc" \
    "$GITHUB_WORKSPACE/.bazelrc" \
    "$workspace_bazelrc_sha256"
  require_file_sha256 \
    "Flywheel Bazel rc" \
    "$GITHUB_WORKSPACE/.bazelrc.flywheel" \
    "$flywheel_bazelrc_sha256"
  require_file_sha256 \
    "Bazel version" \
    "$GITHUB_WORKSPACE/.bazelversion" \
    "$bazel_version_sha256"
  require_file_sha256 "flake" "$GITHUB_WORKSPACE/flake.nix" "$flake_sha256"
  require_file_sha256 "flake lock" "$GITHUB_WORKSPACE/flake.lock" "$flake_lock_sha256"

  require_github_output
  bazelisk_home=$(mktemp -d "$RUNNER_TEMP/bulkload-bazelisk-${BULKLOAD_BAZEL_PHASE}.XXXXXXXX") ||
    die "cannot allocate isolated Bazelisk home"
  chmod 0700 "$bazelisk_home"
  bazel_home="$bazelisk_home/home"
  mkdir -m 0700 "$bazel_home"
  bazel_test_tmpdir="$bazelisk_home/test-tmp"
  mkdir -m 0700 "$bazel_test_tmpdir"
  bazel_runtime_home="$bazelisk_home/runtime-home"
  mkdir -m 0700 "$bazel_runtime_home"
  require_absent_or_empty_file "isolated home Bazel rc" "$bazel_home/.bazelrc"
  require_absent_or_empty_file "isolated home Bazelisk rc" "$bazel_home/.bazeliskrc"
  require_absent_or_empty_file "isolated home netrc" "$bazel_home/.netrc"
  {
    printf 'bazelisk_home=%s\n' "$bazelisk_home"
    printf 'bazel_home=%s\n' "$bazel_home"
    printf 'bazel_test_tmpdir=%s\n' "$bazel_test_tmpdir"
    printf 'bazel_runtime_home=%s\n' "$bazel_runtime_home"
  } >> "$GITHUB_OUTPUT"
  exit 0
fi

require_equal "Attic reachability" "${BULKLOAD_ATTIC_REACHABLE:-}" true
require_equal "Bazel cache reachability" "${BULKLOAD_BAZEL_CACHE_REACHABLE:-}" true
require_equal "Attic cache" "${ATTIC_CACHE:-}" "$cache_name"

{
  emit_common_environment
  printf 'ATTIC_CACHE=%s\n' "$cache_name"
  printf 'ATTIC_PUBLIC_KEY=%s\n' "$public_key"
  printf 'ATTIC_PUBLIC_READ_SITE=%s\n' "$public_site"
  printf 'GF_BAZEL_REMOTE_UPLOAD=%s\n' "$expected_upload"
  printf 'GF_BAZEL_SUBSTRATE_MODE=shared-cache-backed\n'
  printf 'GF_FLYWHEEL_PROFILE_STATE=shared-cache-backed\n'
  printf 'NIX_CONFIG<<BULKLOAD_NIX_CONFIG_%s\n' "$ci_templates_rev"
  printf '%s\n' "$nix_config"
  printf 'BULKLOAD_NIX_CONFIG_%s\n' "$ci_templates_rev"
} >> "$GITHUB_ENV"

require_github_output
{
  printf 'attic_server=%s\n' "$ATTIC_SERVER"
  printf 'bazel_remote_cache=%s\n' "$BAZEL_REMOTE_CACHE"
  printf 'bazel_remote_upload=%s\n' "$expected_upload"
  printf 'trusted_path=%s\n' "$reviewed_step_path"
  printf 'nix_config<<BULKLOAD_NIX_OUTPUT_%s\n' "$ci_templates_rev"
  printf '%s\n' "$nix_config"
  printf 'BULKLOAD_NIX_OUTPUT_%s\n' "$ci_templates_rev"
} >> "$GITHUB_OUTPUT"
