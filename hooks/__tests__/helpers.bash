#!/usr/bin/env bash
# Shared BATS helpers for ctxl hook tests.

_safe_mktemp_d() {
  mktemp -d "${TMPDIR:-/tmp}/ctxl-test.XXXXXX"
}

_assert_temp_path() {
  local p="$1"
  [[ "$p" == "${TMPDIR:-/tmp}"/ctxl-test.* ]]
}
