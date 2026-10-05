// Types for relative imports of detect.mjs (QY1.5 tests in the console).
export declare function workerName(app: string, branch?: string | null): string;
export declare function detect(dir: string): { ok: boolean; framework?: string; build?: string[] | null; outDir?: string; message?: string };
