import { cn } from "@/lib/utils";
import { TONE, type Tone } from "@/lib/model";

// Плашка состояния: всегда значок + подпись, цвет — только усиление (состояние не должно
// зависеть от различения оттенков).
export function StatePlaque({ tone, label, className }: { tone: Tone; label?: string; className?: string }) {
  const t = TONE[tone];
  return (
    <span
      className={cn(
        "inline-flex h-5 shrink-0 items-center gap-1 rounded-full border px-2 text-[11px] font-medium whitespace-nowrap",
        t.chip,
        className,
      )}
    >
      <t.Icon className="size-3" aria-hidden />
      {label ?? t.label}
    </span>
  );
}
