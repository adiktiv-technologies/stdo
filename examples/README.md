# Workflow gallery

Run these commands from the repository root with `stdo` on your PATH. Examples 01 through 06 use POSIX tools and local inputs. Examples 07 through 10 list their extra prerequisites below.

Workflows 01, 04, and 05 also resolve their paths when run from inside `examples/`. Add `--telemetry` (`-t`) to write execution timings and peak monitored buffer usage to stderr.

```sh
cd examples
stdo run -t ./01-parallel-repo-audit.yaml
```

## 01 · Parallel repository audit

Three independent scans count YAML files, lines, and lines matching a literal keyword in `target_dir`. A MiniJinja stdin template combines the results. The grep step accepts exit code 1 so a keyword with no matches yields a count of zero.

```sh
stdo doctor -f examples/01-parallel-repo-audit.yaml
stdo run -f examples/01-parallel-repo-audit.yaml
stdo run -f examples/01-parallel-repo-audit.yaml -i target_dir=./examples -i keyword=does-not-match
```

## 02 · Monitored pipe ETL

An inline dataset flows through successive `pipe_from` links. The last step emits JSON with the record count and the first and last sorted values. The workflow renders those fields as text.

The final step caches its JSON output using the incoming pipe bytes. Run the workflow twice with `-t` to see the cache hit, or pass `--no-cache` to recompute it.

```sh
stdo doctor -f examples/02-monitored-pipe-etl.yaml
stdo run -f examples/02-monitored-pipe-etl.yaml
```

## 03 · Release gate

A JSON boolean drives a switch that selects either `approve_release` or `record_incident`. The selected step prints the simulated decision.

```sh
stdo doctor -f examples/03-release-gate-switch.yaml
stdo run -f examples/03-release-gate-switch.yaml -i severity=LOW
stdo run -f examples/03-release-gate-switch.yaml -i severity=CRITICAL
```

## 04 · Architecture & Safety Self-Audit

Three parallel checks inspect this checkout for unsafe Rust, forbidden network dependency declarations, and Rust source size. Per-file line counts feed the module and line totals through `pipe_from`. A JSON boolean routes the dependency result through a switch. The source and manifest checks are textual and report findings in the summary.

```sh
stdo doctor -f examples/04-stdo-zero-network-guard.yaml
stdo run -f examples/04-stdo-zero-network-guard.yaml
```

## 05 · Repo & Documentation Sync Check

Reads the crate version and Cargo exclusions, checks that each workflow is listed in this guide, and looks for the gallery link and integration test file. The release-readiness card includes a Markdown summary and a JSON block.

```sh
stdo doctor -f examples/05-stdo-release-readiness.yaml
stdo run -f examples/05-stdo-release-readiness.yaml
```

## 06 · JSONL sensor stream

A command emits JSON Lines sensor readings. The next step filters and renames fields over a monitored pipe, then a switch routes on the number of retained records.

```sh
stdo doctor -f examples/06-jsonl-sensor-stream.yaml
stdo run -f examples/06-jsonl-sensor-stream.yaml
```

## 07 · Gemma 4 to Phonon speech

With Ollama running and both models available locally, Gemma 4 answers the prompt and Phonon streams WAV bytes directly to the file writer. The default output is `/tmp/stdo-owl-fact.wav`. Use `-i` to change `llm_model`, `tts_model`, `prompt`, or `output_wav`. [Ollama speech requirements](https://docs.ollama.com/capabilities/speech)

Gemma's two-attempt retry buffers its text before Phonon starts. For live text streaming into Phonon, remove the retry or set `attempts: 1`. The WAV leg streams to a sibling temporary file in either case. A separate step replaces the destination only after Phonon and the staged write succeed. Failed writes clean up their temporary file. A forced kill can leave a staging file behind.

```sh
stdo doctor -f examples/07-ollama-gemma4-phonon-speech.yaml
stdo run -f examples/07-ollama-gemma4-phonon-speech.yaml -i output_wav=./owl-fact.wav
```

## 08 · Local PR triage with Ollama

Two Rust source scans feed a prompt to the local Gemma 4 model. The JSON response routes to `flag_review` or `approve_pr` and prints an advisory verdict. The cache key includes the scan evidence, rendered model argument, and `OLLAMA_HOST`. Use `--no-cache` after updating the weights behind the same model tag.

```sh
stdo doctor -f examples/08-ollama-local-pr-triage.yaml
stdo run -f examples/08-ollama-local-pr-triage.yaml -i target_dir=src
```

## 09 · Resilient HTTP API to JSONL

Install `curl` and `jq`. The default URL fetches recent `stdo` releases from GitHub. Set `api_url` to use any endpoint that returns a JSON array or an object with an `items` array. The workflow retains objects whose numeric `id` is at least `min_id` and validates the normalized JSONL. The atomic pipe starts the file writer only after normalization succeeds and the switch finds records. The writer stages JSONL beside the destination and renames it after a successful write. Empty and invalid responses or failed writes leave the existing destination untouched. The default destination is `/tmp/stdo-api-records.jsonl`.

The speech and HTTP sinks use same-directory rename for atomic replacement. Power-loss durability would also require syncing the file and directory.

```sh
stdo doctor -f examples/09-generic-http-api-pipeline.yaml
stdo run -f examples/09-generic-http-api-pipeline.yaml -i output_jsonl=./releases.jsonl
```

## 10 · Local YouTube capsule

Gemma 4 writes a short narration, Phonon streams WAV bytes to a staged file, and FFmpeg renders an H.264/AAC MP4. With `background_video` set to an existing recording, FFmpeg loops it and fits it to a 1080p frame. Otherwise `render_mp4` creates a compact 360p clip with 48 kHz stereo AAC audio and combines mono audio, dynamic range compression, and square-root scaling to animate a filled cyan waveform over a dark background. Both output files publish by same-directory rename. Studio shows players and download links for both outputs.

Run Ollama with Gemma 4 and Phonon available locally, and put an FFmpeg build with `libx264` and AAC encoding on your PATH. The render step invokes FFmpeg from a shell, so check its availability separately. [Ollama speech guide](https://docs.ollama.com/capabilities/speech) · [FFmpeg documentation](https://ffmpeg.org/ffmpeg.html)

```sh
ffmpeg -version
stdo doctor -f examples/10-youtube-capsule-generator.yaml
stdo run -f examples/10-youtube-capsule-generator.yaml -i topic='How Unix pipes work'
stdo serve -f examples/10-youtube-capsule-generator.yaml
```
