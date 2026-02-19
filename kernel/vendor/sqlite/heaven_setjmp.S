/*
 * setjmp / longjmp for x86_64 bare-metal (no libc).
 *
 * jmp_buf layout (stored at rdi):
 *   [0]  rbx
 *   [8]  rbp
 *   [16] r12
 *   [24] r13
 *   [32] r14
 *   [40] r15
 *   [48] rsp (after return address is popped)
 *   [56] rip (return address)
 *
 * Total: 64 bytes.
 */

.global setjmp
.global _setjmp
.global longjmp
.global _longjmp
.global __sigsetjmp

.text

setjmp:
_setjmp:
__sigsetjmp:
    /* Save callee-saved registers */
    movq %rbx,   (%rdi)
    movq %rbp,  8(%rdi)
    movq %r12, 16(%rdi)
    movq %r13, 24(%rdi)
    movq %r14, 32(%rdi)
    movq %r15, 40(%rdi)
    /* Save stack pointer (value AFTER setjmp returns) */
    leaq 8(%rsp), %rax
    movq %rax,  48(%rdi)
    /* Save return address */
    movq (%rsp), %rax
    movq %rax,  56(%rdi)
    /* Return 0 */
    xorl %eax, %eax
    ret

longjmp:
_longjmp:
    /* val is in esi. If 0, make it 1 */
    movl %esi, %eax
    testl %eax, %eax
    jnz 1f
    incl %eax
1:
    /* Restore callee-saved registers */
    movq   (%rdi), %rbx
    movq  8(%rdi), %rbp
    movq 16(%rdi), %r12
    movq 24(%rdi), %r13
    movq 32(%rdi), %r14
    movq 40(%rdi), %r15
    /* Restore stack pointer */
    movq 48(%rdi), %rsp
    /* Jump to saved return address */
    jmpq *56(%rdi)
