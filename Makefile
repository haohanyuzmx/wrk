fmtCheck:
	cargo fmt --all -- --check

clippyCheck:
	cargo --tests -- -D warnings

precheck: fmtCheck clippyCheck