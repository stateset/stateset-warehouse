import type { UnitInput } from "./types";

export function validUnitInputs(
  value: unknown,
): value is UnitInput[] | undefined {
  if (value === undefined) return true;
  if (!Array.isArray(value) || value.length > 100) return false;
  const targets = new Set<string>();
  const text = (v: unknown) =>
    typeof v === "string" &&
    !!v &&
    v.trim() === v &&
    new TextEncoder().encode(v).length <= 240 &&
    !/[\x00-\x1f\x7f-\x9f]/.test(v);
  return value.every((i) => {
    if (
      !i ||
      Object.keys(i).sort().join() !==
        [
          "target",
          "sku",
          "pack_code",
          "definition_id",
          "pack_quantity",
          "each_per_pack",
        ]
          .sort()
          .join() ||
      !Array.isArray(i.target) ||
      i.target.length > 3 ||
      !i.target.every(text) ||
      !text(i.sku) ||
      !text(i.definition_id) ||
      typeof i.pack_code !== "string" ||
      !/^[A-Z0-9_-]{1,32}$/.test(i.pack_code) ||
      i.pack_code === "EA" ||
      !Number.isSafeInteger(i.pack_quantity) ||
      i.pack_quantity < 0 ||
      i.pack_quantity > 1000000 ||
      !Number.isSafeInteger(i.each_per_pack) ||
      i.each_per_pack < 1 ||
      i.each_per_pack > 1000000 ||
      i.pack_quantity * i.each_per_pack > 1000000
    )
      return false;
    const key = JSON.stringify(i.target);
    if (targets.has(key)) return false;
    targets.add(key);
    return true;
  });
}
