#!/usr/bin/env bash
# Compile and run the SDK checks with any Kotlin compiler on the machine:
# `kotlinc` when installed, else the compiler embedded in a Gradle
# distribution (GRADLE_LIB, or the newest one under ~/.gradle/wrapper/dists).
set -euo pipefail
cd "$(dirname "$0")/.."
OUT="${TMPDIR:-/tmp}/railyard-analytics-kotlin-build"
rm -rf "$OUT" && mkdir -p "$OUT"
SOURCES=$(find src/main/kotlin src/test/kotlin -name '*.kt')
if command -v kotlinc >/dev/null 2>&1; then
  kotlinc $SOURCES -d "$OUT" -jvm-target 17 2>&1 | grep -v '^warning' || true
  STDLIB=$(dirname "$(command -v kotlinc)")/../lib/kotlin-stdlib.jar
else
  LIB="${GRADLE_LIB:-$(ls -d "$HOME"/.gradle/wrapper/dists/gradle-*/*/gradle-*/lib 2>/dev/null | sort -V | tail -1)}"
  COMPILER=$(ls "$LIB"/kotlin-compiler-embeddable-*.jar | head -1)
  STDLIB=$(ls "$LIB"/kotlin-stdlib-[0-9]*.jar | head -1)
  CP="$COMPILER:$STDLIB:$(ls "$LIB"/kotlin-reflect-*.jar | head -1):$(ls "$LIB"/kotlin-script-runtime-*.jar | head -1):$(ls "$LIB"/kotlinx-coroutines-core-jvm-*.jar | head -1):$(ls "$LIB"/annotations-*.jar | head -1):$(ls "$LIB"/kotlin-daemon-embeddable-*.jar | head -1)"
  java -cp "$CP" org.jetbrains.kotlin.cli.jvm.K2JVMCompiler -no-stdlib -no-reflect -classpath "$STDLIB" -jvm-target 17 -d "$OUT" $SOURCES
fi
java -cp "$OUT:$STDLIB" com.railyard.analytics.AnalyticsTestKt
