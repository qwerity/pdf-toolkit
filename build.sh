#!/usr/bin/env bash
set -euo pipefail

usage() {
    cat <<'EOF'
Usage: ./build.sh [macos|mac|linux|windows] [--with-artifacts]

Builds a runtime bundle in ./dist that contains only:
- the pdf_cutter executable
- the matching PDFium native library

Pass --with-artifacts to also write release archives and SHA-256 checksum files to ./artifacts.

Environment variables:
- RELEASE_TAG   Optional label used in archive names. Defaults to v<Cargo.toml version>.
- SKIP_BUILD    Set to 1 to skip `cargo build --locked --release`.
EOF
}

read_cargo_value() {
    local key="$1"
    sed -nE "s/^${key} = \"([^\"]+)\"/\1/p" Cargo.toml | head -n 1
}

detect_platform() {
    case "$(uname -s)" in
        Darwin)
            echo "macos"
            ;;
        Linux)
            echo "linux"
            ;;
        MINGW* | MSYS* | CYGWIN*)
            echo "windows"
            ;;
        *)
            echo "Could not auto-detect platform. Pass one of: macos, linux, windows." >&2
            return 1
            ;;
    esac
}

normalize_platform() {
    case "${1:-}" in
        "")
            detect_platform
            ;;
        mac | macos)
            echo "macos"
            ;;
        linux)
            echo "linux"
            ;;
        windows | win)
            echo "windows"
            ;;
        -h | --help | help)
            usage
            exit 0
            ;;
        *)
            echo "Unsupported platform: ${1}" >&2
            usage >&2
            return 1
            ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64 | amd64)
            echo "x64"
            ;;
        arm64 | aarch64)
            echo "arm64"
            ;;
        *)
            echo "Unsupported architecture: $(uname -m)" >&2
            return 1
            ;;
    esac
}

platform_library_name() {
    case "$1" in
        macos)
            echo "libpdfium.dylib"
            ;;
        linux)
            echo "libpdfium.so"
            ;;
        windows)
            echo "pdfium.dll"
            ;;
    esac
}

parse_args() {
    local raw_platform=""
    local arg=""
    CREATE_ARTIFACTS=0

    for arg in "$@"; do
        case "${arg}" in
            --with-artifacts)
                CREATE_ARTIFACTS=1
                ;;
            -h | --help | help)
                usage
                exit 0
                ;;
            *)
                if [[ -n "${raw_platform}" ]]; then
                    echo "Unexpected argument: ${arg}" >&2
                    usage >&2
                    exit 1
                fi
                raw_platform="${arg}"
                ;;
        esac
    done

    PLATFORM="$(normalize_platform "${raw_platform}")"
}

download_pdfium() {
    local platform="$1"
    local arch="$2"
    local temp_dir="$3"
    local asset=""
    local library_path=""

    case "$platform" in
        macos)
            asset="pdfium-mac-${arch}.tgz"
            library_path="lib/libpdfium.dylib"
            ;;
        linux)
            asset="pdfium-linux-${arch}.tgz"
            library_path="lib/libpdfium.so"
            ;;
        windows)
            asset="pdfium-win-${arch}.tgz"
            library_path="bin/pdfium.dll"
            ;;
    esac

    local url="https://github.com/bblanchon/pdfium-binaries/releases/latest/download/${asset}"
    local archive_path="${temp_dir}/${asset}"

    echo "Downloading ${asset}..." >&2
    curl --fail --location --silent --show-error --output "${archive_path}" "${url}"
    tar -xzf "${archive_path}" -C "${temp_dir}" "${library_path}"
    printf '%s\n' "${temp_dir}/${library_path}"
}

create_archive() {
    local artifacts_dir="$1"
    local package_root="$2"
    local package_dir_name="$3"
    local platform="$4"
    local archive_path=""

    if [[ "${platform}" == "windows" ]]; then
        archive_path="${artifacts_dir}/${package_dir_name}.zip"
        if command -v 7z >/dev/null 2>&1; then
            (
                cd "${package_root}"
                7z a -tzip "${package_dir_name}.zip" "./${package_dir_name}" >/dev/null
            )
            mv "${package_root}/${package_dir_name}.zip" "${archive_path}"
        else
            (
                cd "${package_root}"
                pwsh -NoLogo -NoProfile -Command \
                    "Compress-Archive -Path './${package_dir_name}' -DestinationPath './${package_dir_name}.zip' -Force" >/dev/null
            )
            mv "${package_root}/${package_dir_name}.zip" "${archive_path}"
        fi
    else
        archive_path="${artifacts_dir}/${package_dir_name}.tar.gz"
        tar -czf "${archive_path}" -C "${package_root}" "${package_dir_name}"
    fi

    printf '%s\n' "${archive_path}"
}

write_checksum() {
    local archive_path="$1"
    local archive_dir
    local archive_name
    local checksum_name

    archive_dir="$(dirname "${archive_path}")"
    archive_name="$(basename "${archive_path}")"
    checksum_name="${archive_name}.sha256"

    (
        cd "${archive_dir}"
        if command -v sha256sum >/dev/null 2>&1; then
            sha256sum "${archive_name}" > "${checksum_name}"
        else
            shasum -a 256 "${archive_name}" > "${checksum_name}"
        fi
    )

    printf '%s\n' "${archive_dir}/${checksum_name}"
}

clear_dir() {
    local dir="$1"

    mkdir -p "${dir}"
    find "${dir}" -mindepth 1 -maxdepth 1 -exec rm -rf {} +
}

prune_runtime_bundle() {
    local dir="$1"
    local binary_name="$2"
    local library_name="$3"

    mkdir -p "${dir}"
    find "${dir}" -mindepth 1 -maxdepth 1 \
        ! -name "${binary_name}" \
        ! -name "${library_name}" \
        -exec rm -rf {} +
}

ensure_temp_dir() {
    if [[ -n "${TEMP_DIR:-}" ]]; then
        return
    fi

    TEMP_DIR="$(mktemp -d)"
    trap "rm -rf '${TEMP_DIR}'" EXIT
}

main() {
    local arch
    local crate_name
    local crate_version
    local release_tag
    local binary_name="pdf_cutter"
    local dist_dir="dist"
    local artifacts_dir="artifacts"
    local create_artifacts
    local package_dir_name
    local package_root
    local package_dir
    local library_name
    local dist_library_path
    local pdfium_library
    local archive_path
    local checksum_path

    parse_args "$@"
    arch="$(detect_arch)"
    create_artifacts="${CREATE_ARTIFACTS}"

    crate_name="$(read_cargo_value "name")"
    crate_version="$(read_cargo_value "version")"
    release_tag="${RELEASE_TAG:-v${crate_version}}"

    if [[ -z "${crate_name}" || -z "${crate_version}" ]]; then
        echo "Could not read package metadata from Cargo.toml." >&2
        exit 1
    fi

    if [[ "${PLATFORM}" == "windows" ]]; then
        binary_name="${binary_name}.exe"
    fi

    library_name="$(platform_library_name "${PLATFORM}")"
    dist_library_path="${dist_dir}/${library_name}"
    package_dir_name="${crate_name}-${release_tag}-${PLATFORM}-${arch}"

    echo "=== Packaging ${crate_name} ${release_tag} for ${PLATFORM}/${arch} ==="

    if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
        echo "1. Building Rust CLI..."
        cargo build --locked --release
    else
        echo "1. Skipping build because SKIP_BUILD=1"
    fi

    echo "2. Preparing runtime bundle..."
    prune_runtime_bundle "${dist_dir}" "${binary_name}" "${library_name}"

    echo "3. Copying binary..."
    cp "target/release/${binary_name}" "${dist_dir}/${binary_name}"

    if [[ -f "${dist_library_path}" ]]; then
        echo "4. Reusing ${library_name} from ${dist_dir}"
    else
        echo "4. Downloading ${library_name}..."
        ensure_temp_dir
        pdfium_library="$(download_pdfium "${PLATFORM}" "${arch}" "${TEMP_DIR}")"
        cp "${pdfium_library}" "${dist_library_path}"
    fi

    if [[ "${create_artifacts}" == "1" ]]; then
        echo "5. Creating release artifacts..."
        clear_dir "${artifacts_dir}"
        ensure_temp_dir
        package_root="${TEMP_DIR}/package"
        package_dir="${package_root}/${package_dir_name}"
        rm -rf "${package_root}"
        mkdir -p "${package_dir}"
        cp "${dist_dir}/${binary_name}" "${package_dir}/${binary_name}"
        cp "${dist_library_path}" "${package_dir}/${library_name}"

        archive_path="$(create_archive "${artifacts_dir}" "${package_root}" "${package_dir_name}" "${PLATFORM}")"
        checksum_path="$(write_checksum "${archive_path}")"
    fi

    echo "=== Done ==="
    echo "Runtime bundle: ${dist_dir}"
    if [[ "${create_artifacts}" == "1" ]]; then
        echo "Archive: ${archive_path}"
        echo "Checksum: ${checksum_path}"
    else
        echo "Release artifacts: skipped"
    fi
}

main "$@"
