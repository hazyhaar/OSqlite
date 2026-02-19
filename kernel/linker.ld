/* HeavenOS kernel linker script — x86_64 higher-half kernel */

ENTRY(_start)

KERNEL_VIRT_BASE = 0xFFFFFFFF80000000; /* -2 GiB, classic higher-half */

SECTIONS
{
    . = KERNEL_VIRT_BASE;

    .text ALIGN(4K) :
    {
        KEEP(*(.text.boot))    /* Boot entry point first */
        *(.text .text.*)
    }

    .rodata ALIGN(4K) :
    {
        *(.rodata .rodata.*)
    }

    .data ALIGN(4K) :
    {
        *(.data .data.*)
    }

    .bss ALIGN(4K) :
    {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        __bss_end = .;
    }

    __kernel_end = .;

    /DISCARD/ :
    {
        *(.eh_frame)
        *(.note .note.*)
        *(.comment)
    }
}
