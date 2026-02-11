import { Switch } from '@/components/ui/switch'

// Checkbox or switch component
type CheckboxControlProps = {
  checked: boolean
  onChange: (checked: boolean) => void
  disabled?: boolean
}

export function CheckboxControl({
  checked,
  onChange,
  disabled = false,
}: CheckboxControlProps) {
  return (
    <Switch
      checked={checked}
      disabled={disabled}
      onCheckedChange={(value) => onChange(value)}
    />
  )
}
