// The GitHub repository of a folder, from its `origin` remote (https or ssh).

import { git } from "../run.mjs";

const PATTERNS = [/^https:\/\/github\.com\/([^/]+)\/([^/]+?)(?:\.git)?\/?$/i, /^git@github\.com:([^/]+)\/([^/]+?)(?:\.git)?$/i, /^ssh:\/\/git@github\.com\/([^/]+)\/([^/]+?)(?:\.git)?$/i];

export function githubRepo(remoteUrl) {
  const url = String(remoteUrl ?? "").trim().replace(/^https:\/\/[^@/]+@github\.com\//i, "https://github.com/");
  for (const re of PATTERNS) {
    const m = re.exec(url);
    if (m) return `${m[1]}/${m[2]}`;
  }
  return null;
}

export function repoOf(dir) {
  return githubRepo(git(dir, ["remote", "get-url", "origin"]));
}
