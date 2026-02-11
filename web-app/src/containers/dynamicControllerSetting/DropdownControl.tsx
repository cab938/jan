import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'

import { cn } from '@/lib/utils'
import { Button } from '@/components/ui/button'
import { ChevronsUpDown } from 'lucide-react'

// Dropdown component
type DropdownControlProps = {
  value: string
  options?: Array<{ value: number | string; name: string }>
  recommended?: string
  onChange: (value: number | string) => void
  disabled?: boolean
}

export function DropdownControl({
  value,
  options = [],
  onChange,
  disabled = false,
}: DropdownControlProps) {
  const isSelected =
    options.find((option) => option.value === value)?.name || value

  return (
    <DropdownMenu>
      <DropdownMenuTrigger disabled={disabled}>
        <Button
          variant="outline"
          size="sm"
          className="w-full justify-between"
          disabled={disabled}
        >
          {isSelected}
          <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground ml-2" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="max-h-70">
        {options.map((option, optionIndex) => (
          <DropdownMenuItem
            key={optionIndex}
            onClick={() => {
              if (!disabled) onChange(option.value)
            }}
            disabled={disabled}
            className={cn(
              'flex items-center justify-between my-1',
              isSelected === option.name
                ? 'bg-secondary/60 hover:bg-secondary/40'
                : ''
            )}
          >
            <span>{option.name}</span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
