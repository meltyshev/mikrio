import js from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      // Build output
      "**/dist/**",
      "**/test-dist/**",
      "artifacts/**",

      // Dependencies and caches
      "**/node_modules/**",
      ".cache/**",
    ],
  },
  {
    files: [
      "packages/node/{src,tests}/**/*.{cts,mts}",
      "tests/sdk/node/src/**/*.mts",
      "examples/node/attachments/{src,tests}/**/*.mts",
    ],
    extends: [
      js.configs.recommended,
      ...tseslint.configs.recommendedTypeChecked,
    ],
    languageOptions: {
      parserOptions: {
        project: [
          "packages/node/tsconfig.json",
          "packages/node/tsconfig.test.json",
          "tests/sdk/node/tsconfig.json",
          "examples/node/attachments/tsconfig.json",
        ],
        tsconfigRootDir: import.meta.dirname,
      },
    },
    rules: {
      "@typescript-eslint/no-floating-promises": "error",
      "@typescript-eslint/no-misused-promises": "error",
      "@typescript-eslint/no-non-null-assertion": "error",
    },
  },
  {
    files: ["packages/node/{src,tests}/**/*.{cts,mts}"],
    rules: {
      curly: ["error", "all"],
    },
  },
  {
    files: ["packages/node/tests/types.mts"],
    rules: {
      // Compiler-only negative examples intentionally describe invalid calls.
      "@typescript-eslint/no-floating-promises": "off",
    },
  },
);
