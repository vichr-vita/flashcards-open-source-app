import { Slot } from "@radix-ui/react-slot";
import { cva, type VariantProps } from "class-variance-authority";
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
import type { ComponentProps, ReactElement } from "react";

export function cn(...inputs: Array<ClassValue>): string {
  return twMerge(clsx(inputs));
}

// The shadcn composition API uses the existing CSS classes to preserve the shipped design.
const buttonVariants = cva("", {
  variants: { variant: { default: "primary-btn", ghost: "ghost-btn" } },
  defaultVariants: { variant: "default" },
});

export function Button({
  className,
  variant,
  asChild = false,
  ...props
}: ComponentProps<"button"> & VariantProps<typeof buttonVariants> & Readonly<{ asChild?: boolean }>): ReactElement {
  const Component = asChild ? Slot : "button";
  return <Component {...props} className={cn(buttonVariants({ variant }), className)} />;
}
