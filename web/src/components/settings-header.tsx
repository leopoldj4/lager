import { Separator } from "@/components/ui/separator"

/** The heading every settings section starts with. */
export function SettingsHeader({
  title,
  description,
}: {
  title: string
  description: string
}) {
  return (
    <>
      <div className="grid gap-1">
        <h2 className="text-lg font-medium">{title}</h2>
        <p className="text-sm text-muted-foreground">{description}</p>
      </div>
      <Separator />
    </>
  )
}
