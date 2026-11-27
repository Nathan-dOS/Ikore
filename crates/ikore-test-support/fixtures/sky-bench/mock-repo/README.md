# Parcel clearinghouse

The Rust ledger admits bounded batches and quotes handling fees in cents. The console previews the same limits, the data jobs reconcile receipts, the native scanner validates counts and the carrier bridge exports a settlement. Each area owns a separately versioned tariff.

Run cargo test, node --test tools/*.test.js, python -m unittest discover -s jobs, dotnet run --project integration and ctest from a built native directory. TypeScript checks run with npx tsx --test ui/*.test.ts.
