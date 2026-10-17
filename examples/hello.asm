main:
; func main
  100000380:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  100000384:  fd 03 00 91                    add x29, sp, #0
  100000388:  ff 83 00 d1                    sub sp, sp, #32
.L0:
; block entry
  10000038c:  09 00 00 90                    adrp x9, str.0@PAGE
  100000390:  29 01 00 91                    add x9, x9, str.0@PAGEOFF
  100000394:  e9 03 00 f9                    str x9, [sp]
  100000398:  e0 03 40 f9                    ldr x0, [sp]
  10000039c:  00 00 00 94                    bl rt.print_str
  1000003a0:  00 00 00 94                    bl rt.print_newline
  1000003a4:  bf 03 00 91                    add sp, x29, #0
  1000003a8:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  1000003ac:  c0 03 5f d6                    ret

luna.main:
; func luna.main
  1000003b0:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  1000003b4:  fd 03 00 91                    add x29, sp, #0
.L0:
; block entry
  1000003b8:  00 00 00 94                    bl main
  1000003bc:  bf 03 00 91                    add sp, x29, #0
  1000003c0:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  1000003c4:  c0 03 5f d6                    ret

rt.exit:
; func rt.exit
  1000003d0:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  1000003d4:  fd 03 00 91                    add x29, sp, #0
  1000003d8:  ff 43 00 d1                    sub sp, sp, #16
  1000003dc:  e0 03 00 f9                    str x0, [sp]
.L0:
; block entry
  1000003e0:  00 00 00 94                    bl rt.flush
  1000003e4:  e0 03 40 f9                    ldr x0, [sp]
  1000003e8:  00 00 00 94                    bl rt.sys_exit
  1000003ec:  20 00 20 d4                    brk #1

rt.memcpy:
; func rt.memcpy
  1000003f0:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  1000003f4:  fd 03 00 91                    add x29, sp, #0
  1000003f8:  ff 83 01 d1                    sub sp, sp, #96
  1000003fc:  e0 03 00 f9                    str x0, [sp]
  100000400:  e1 07 00 f9                    str x1, [sp, #8]
  100000404:  e2 0b 00 f9                    str x2, [sp, #16]
.L0:
; block entry
  100000408:  09 00 80 d2                    movz x9, #0x0, lsl #0
  10000040c:  e9 0f 00 f9                    str x9, [sp, #24]
  100000410:  29 00 80 d2                    movz x9, #0x1, lsl #0
  100000414:  e9 13 00 f9                    str x9, [sp, #32]
  100000418:  e9 0f 40 f9                    ldr x9, [sp, #24]
  10000041c:  e9 17 00 f9                    str x9, [sp, #40]
.L1:
; block loop
  100000420:  e9 17 40 f9                    ldr x9, [sp, #40]
  100000424:  ea 0b 40 f9                    ldr x10, [sp, #16]
  100000428:  3f 01 0a eb                    cmp x9, x10
  10000042c:  e9 a7 9f 9a                    cset x9, lt
  100000430:  e9 1b 00 f9                    str x9, [sp, #48]
  100000434:  e9 1b 40 f9                    ldr x9, [sp, #48]
  100000438:  49 00 00 b5                    cbnz x9, .L2
  10000043c:  16 00 00 14                    b .L3
.L2:
; block body
  100000440:  e9 07 40 f9                    ldr x9, [sp, #8]
  100000444:  ea 17 40 f9                    ldr x10, [sp, #40]
  100000448:  29 01 0a 8b                    add x9, x9, x10
  10000044c:  e9 1f 00 f9                    str x9, [sp, #56]
  100000450:  e9 1f 40 f9                    ldr x9, [sp, #56]
  100000454:  29 01 40 39                    ldrb w9, [x9]
  100000458:  e9 23 00 f9                    str x9, [sp, #64]
  10000045c:  e9 03 40 f9                    ldr x9, [sp]
  100000460:  ea 17 40 f9                    ldr x10, [sp, #40]
  100000464:  29 01 0a 8b                    add x9, x9, x10
  100000468:  e9 27 00 f9                    str x9, [sp, #72]
  10000046c:  e9 27 40 f9                    ldr x9, [sp, #72]
  100000470:  ea 23 40 f9                    ldr x10, [sp, #64]
  100000474:  2a 01 00 39                    strb w10, [x9]
  100000478:  e9 17 40 f9                    ldr x9, [sp, #40]
  10000047c:  ea 13 40 f9                    ldr x10, [sp, #32]
  100000480:  29 01 0a 8b                    add x9, x9, x10
  100000484:  e9 2b 00 f9                    str x9, [sp, #80]
  100000488:  e9 2b 40 f9                    ldr x9, [sp, #80]
  10000048c:  e9 17 00 f9                    str x9, [sp, #40]
  100000490:  e4 ff ff 17                    b .L1
.L3:
; block done
  100000494:  bf 03 00 91                    add sp, x29, #0
  100000498:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  10000049c:  c0 03 5f d6                    ret

rt.write:
; func rt.write
  1000004a0:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  1000004a4:  fd 03 00 91                    add x29, sp, #0
  1000004a8:  ff c3 00 d1                    sub sp, sp, #48
  1000004ac:  e0 03 00 f9                    str x0, [sp]
  1000004b0:  e1 07 00 f9                    str x1, [sp, #8]
  1000004b4:  e2 0b 00 f9                    str x2, [sp, #16]
.L0:
; block entry
  1000004b8:  09 00 80 d2                    movz x9, #0x0, lsl #0
  1000004bc:  e9 0f 00 f9                    str x9, [sp, #24]
  1000004c0:  e9 0b 40 f9                    ldr x9, [sp, #16]
  1000004c4:  ea 0f 40 f9                    ldr x10, [sp, #24]
  1000004c8:  3f 01 0a eb                    cmp x9, x10
  1000004cc:  e9 c7 9f 9a                    cset x9, le
  1000004d0:  e9 13 00 f9                    str x9, [sp, #32]
  1000004d4:  e9 13 40 f9                    ldr x9, [sp, #32]
  1000004d8:  09 01 00 b5                    cbnz x9, .L2
.L1:
; block write
  1000004dc:  e0 03 40 f9                    ldr x0, [sp]
  1000004e0:  e1 07 40 f9                    ldr x1, [sp, #8]
  1000004e4:  e2 0b 40 f9                    ldr x2, [sp, #16]
  1000004e8:  00 00 00 94                    bl rt.sys_write
  1000004ec:  bf 03 00 91                    add sp, x29, #0
  1000004f0:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  1000004f4:  c0 03 5f d6                    ret
.L2:
; block done
  1000004f8:  bf 03 00 91                    add sp, x29, #0
  1000004fc:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  100000500:  c0 03 5f d6                    ret

rt.flush:
; func rt.flush
  100000510:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  100000514:  fd 03 00 91                    add x29, sp, #0
  100000518:  ff c3 00 d1                    sub sp, sp, #48
.L0:
; block entry
  10000051c:  09 00 00 90                    adrp x9, rt.outbuf@PAGE
  100000520:  29 01 00 91                    add x9, x9, rt.outbuf@PAGEOFF
  100000524:  e9 03 00 f9                    str x9, [sp]
  100000528:  e9 03 40 f9                    ldr x9, [sp]
  10000052c:  29 01 40 f9                    ldr x9, [x9]
  100000530:  e9 07 00 f9                    str x9, [sp, #8]
  100000534:  29 00 80 d2                    movz x9, #0x1, lsl #0
  100000538:  e9 0b 00 f9                    str x9, [sp, #16]
  10000053c:  09 01 80 d2                    movz x9, #0x8, lsl #0
  100000540:  e9 0f 00 f9                    str x9, [sp, #24]
  100000544:  e9 03 40 f9                    ldr x9, [sp]
  100000548:  ea 0f 40 f9                    ldr x10, [sp, #24]
  10000054c:  29 01 0a 8b                    add x9, x9, x10
  100000550:  e9 13 00 f9                    str x9, [sp, #32]
  100000554:  e0 0b 40 f9                    ldr x0, [sp, #16]
  100000558:  e1 13 40 f9                    ldr x1, [sp, #32]
  10000055c:  e2 07 40 f9                    ldr x2, [sp, #8]
  100000560:  00 00 00 94                    bl rt.write
  100000564:  09 00 80 d2                    movz x9, #0x0, lsl #0
  100000568:  e9 17 00 f9                    str x9, [sp, #40]
  10000056c:  e9 03 40 f9                    ldr x9, [sp]
  100000570:  ea 17 40 f9                    ldr x10, [sp, #40]
  100000574:  2a 01 00 f9                    str x10, [x9]
  100000578:  bf 03 00 91                    add sp, x29, #0
  10000057c:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  100000580:  c0 03 5f d6                    ret

rt.out:
; func rt.out
  100000590:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  100000594:  fd 03 00 91                    add x29, sp, #0
  100000598:  ff c3 01 d1                    sub sp, sp, #112
  10000059c:  e0 03 00 f9                    str x0, [sp]
  1000005a0:  e1 07 00 f9                    str x1, [sp, #8]
.L0:
; block entry
  1000005a4:  09 00 82 d2                    movz x9, #0x1000, lsl #0
  1000005a8:  e9 0b 00 f9                    str x9, [sp, #16]
  1000005ac:  09 00 00 90                    adrp x9, rt.outbuf@PAGE
  1000005b0:  29 01 00 91                    add x9, x9, rt.outbuf@PAGEOFF
  1000005b4:  e9 0f 00 f9                    str x9, [sp, #24]
  1000005b8:  09 01 80 d2                    movz x9, #0x8, lsl #0
  1000005bc:  e9 13 00 f9                    str x9, [sp, #32]
  1000005c0:  e9 07 40 f9                    ldr x9, [sp, #8]
  1000005c4:  ea 0b 40 f9                    ldr x10, [sp, #16]
  1000005c8:  3f 01 0a eb                    cmp x9, x10
  1000005cc:  e9 d7 9f 9a                    cset x9, gt
  1000005d0:  e9 17 00 f9                    str x9, [sp, #40]
  1000005d4:  e9 17 40 f9                    ldr x9, [sp, #40]
  1000005d8:  49 00 00 b5                    cbnz x9, .L1
  1000005dc:  0b 00 00 14                    b .L2
.L1:
; block direct
  1000005e0:  00 00 00 94                    bl rt.flush
  1000005e4:  29 00 80 d2                    movz x9, #0x1, lsl #0
  1000005e8:  e9 1b 00 f9                    str x9, [sp, #48]
  1000005ec:  e0 1b 40 f9                    ldr x0, [sp, #48]
  1000005f0:  e1 03 40 f9                    ldr x1, [sp]
  1000005f4:  e2 07 40 f9                    ldr x2, [sp, #8]
  1000005f8:  00 00 00 94                    bl rt.write
  1000005fc:  bf 03 00 91                    add sp, x29, #0
  100000600:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  100000604:  c0 03 5f d6                    ret
.L2:
; block buffered
  100000608:  e9 0f 40 f9                    ldr x9, [sp, #24]
  10000060c:  29 01 40 f9                    ldr x9, [x9]
  100000610:  e9 1f 00 f9                    str x9, [sp, #56]
  100000614:  e9 1f 40 f9                    ldr x9, [sp, #56]
  100000618:  ea 07 40 f9                    ldr x10, [sp, #8]
  10000061c:  29 01 0a 8b                    add x9, x9, x10
  100000620:  e9 23 00 f9                    str x9, [sp, #64]
  100000624:  e9 23 40 f9                    ldr x9, [sp, #64]
  100000628:  ea 0b 40 f9                    ldr x10, [sp, #16]
  10000062c:  3f 01 0a eb                    cmp x9, x10
  100000630:  e9 d7 9f 9a                    cset x9, gt
  100000634:  e9 27 00 f9                    str x9, [sp, #72]
  100000638:  e9 27 40 f9                    ldr x9, [sp, #72]
  10000063c:  49 00 00 b5                    cbnz x9, .L3
  100000640:  02 00 00 14                    b .L4
.L3:
; block flush_first
  100000644:  00 00 00 94                    bl rt.flush
.L4:
; block copy
  100000648:  e9 0f 40 f9                    ldr x9, [sp, #24]
  10000064c:  29 01 40 f9                    ldr x9, [x9]
  100000650:  e9 2b 00 f9                    str x9, [sp, #80]
  100000654:  e9 0f 40 f9                    ldr x9, [sp, #24]
  100000658:  ea 13 40 f9                    ldr x10, [sp, #32]
  10000065c:  29 01 0a 8b                    add x9, x9, x10
  100000660:  e9 2f 00 f9                    str x9, [sp, #88]
  100000664:  e9 2f 40 f9                    ldr x9, [sp, #88]
  100000668:  ea 2b 40 f9                    ldr x10, [sp, #80]
  10000066c:  29 01 0a 8b                    add x9, x9, x10
  100000670:  e9 33 00 f9                    str x9, [sp, #96]
  100000674:  e0 33 40 f9                    ldr x0, [sp, #96]
  100000678:  e1 03 40 f9                    ldr x1, [sp]
  10000067c:  e2 07 40 f9                    ldr x2, [sp, #8]
  100000680:  00 00 00 94                    bl rt.memcpy
  100000684:  e9 2b 40 f9                    ldr x9, [sp, #80]
  100000688:  ea 07 40 f9                    ldr x10, [sp, #8]
  10000068c:  29 01 0a 8b                    add x9, x9, x10
  100000690:  e9 37 00 f9                    str x9, [sp, #104]
  100000694:  e9 0f 40 f9                    ldr x9, [sp, #24]
  100000698:  ea 37 40 f9                    ldr x10, [sp, #104]
  10000069c:  2a 01 00 f9                    str x10, [x9]
  1000006a0:  bf 03 00 91                    add sp, x29, #0
  1000006a4:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  1000006a8:  c0 03 5f d6                    ret

rt.out_byte:
; func rt.out_byte
  1000006b0:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  1000006b4:  fd 03 00 91                    add x29, sp, #0
  1000006b8:  ff 83 00 d1                    sub sp, sp, #32
  1000006bc:  e0 03 00 f9                    str x0, [sp]
.L0:
; block entry
  1000006c0:  e9 63 00 91                    add x9, sp, #24
  1000006c4:  e9 07 00 f9                    str x9, [sp, #8]
  1000006c8:  e9 07 40 f9                    ldr x9, [sp, #8]
  1000006cc:  ea 03 40 f9                    ldr x10, [sp]
  1000006d0:  2a 01 00 39                    strb w10, [x9]
  1000006d4:  29 00 80 d2                    movz x9, #0x1, lsl #0
  1000006d8:  e9 0b 00 f9                    str x9, [sp, #16]
  1000006dc:  e0 07 40 f9                    ldr x0, [sp, #8]
  1000006e0:  e1 0b 40 f9                    ldr x1, [sp, #16]
  1000006e4:  00 00 00 94                    bl rt.out
  1000006e8:  bf 03 00 91                    add sp, x29, #0
  1000006ec:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  1000006f0:  c0 03 5f d6                    ret

rt.print_str:
; func rt.print_str
  100000700:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  100000704:  fd 03 00 91                    add x29, sp, #0
  100000708:  ff 83 00 d1                    sub sp, sp, #32
  10000070c:  e0 03 00 f9                    str x0, [sp]
.L0:
; block entry
  100000710:  e9 03 40 f9                    ldr x9, [sp]
  100000714:  29 01 40 f9                    ldr x9, [x9]
  100000718:  e9 07 00 f9                    str x9, [sp, #8]
  10000071c:  09 01 80 d2                    movz x9, #0x8, lsl #0
  100000720:  e9 0b 00 f9                    str x9, [sp, #16]
  100000724:  e9 03 40 f9                    ldr x9, [sp]
  100000728:  ea 0b 40 f9                    ldr x10, [sp, #16]
  10000072c:  29 01 0a 8b                    add x9, x9, x10
  100000730:  e9 0f 00 f9                    str x9, [sp, #24]
  100000734:  e0 0f 40 f9                    ldr x0, [sp, #24]
  100000738:  e1 07 40 f9                    ldr x1, [sp, #8]
  10000073c:  00 00 00 94                    bl rt.out
  100000740:  bf 03 00 91                    add sp, x29, #0
  100000744:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  100000748:  c0 03 5f d6                    ret

rt.print_newline:
; func rt.print_newline
  100000750:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  100000754:  fd 03 00 91                    add x29, sp, #0
  100000758:  ff 43 00 d1                    sub sp, sp, #16
.L0:
; block entry
  10000075c:  49 01 80 d2                    movz x9, #0xa, lsl #0
  100000760:  e9 03 00 f9                    str x9, [sp]
  100000764:  e0 03 40 f9                    ldr x0, [sp]
  100000768:  00 00 00 94                    bl rt.out_byte
  10000076c:  bf 03 00 91                    add sp, x29, #0
  100000770:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  100000774:  c0 03 5f d6                    ret

rt.start:
; func rt.start
  100000780:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  100000784:  fd 03 00 91                    add x29, sp, #0
  100000788:  ff 43 00 d1                    sub sp, sp, #16
.L0:
; block entry
  10000078c:  00 00 00 94                    bl luna.main
  100000790:  09 00 80 d2                    movz x9, #0x0, lsl #0
  100000794:  e9 03 00 f9                    str x9, [sp]
  100000798:  e0 03 40 f9                    ldr x0, [sp]
  10000079c:  00 00 00 94                    bl rt.exit
  1000007a0:  20 00 20 d4                    brk #1

rt.sys_exit:
; func rt.sys_exit
  1000007b0:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  1000007b4:  fd 03 00 91                    add x29, sp, #0
  1000007b8:  ff 43 00 d1                    sub sp, sp, #16
  1000007bc:  e0 03 00 f9                    str x0, [sp]
.L0:
; block entry
  1000007c0:  e0 03 40 f9                    ldr x0, [sp]
  1000007c4:  10 00 00 90                    adrp x16, __imp.libSystem.exit@PAGE
  1000007c8:  10 02 40 f9                    ldr x16, [x16, __imp.libSystem.exit@PAGEOFF]
  1000007cc:  00 02 3f d6                    blr x16
  1000007d0:  20 00 20 d4                    brk #1

rt.sys_write:
; func rt.sys_write
  1000007e0:  fd 7b bf a9                    stp x29, x30, [sp, #-16]!
  1000007e4:  fd 03 00 91                    add x29, sp, #0
  1000007e8:  ff c3 01 d1                    sub sp, sp, #112
  1000007ec:  e0 03 00 f9                    str x0, [sp]
  1000007f0:  e1 07 00 f9                    str x1, [sp, #8]
  1000007f4:  e2 0b 00 f9                    str x2, [sp, #16]
.L0:
; block entry
  1000007f8:  09 00 80 d2                    movz x9, #0x0, lsl #0
  1000007fc:  e9 0f 00 f9                    str x9, [sp, #24]
  100000800:  e9 07 40 f9                    ldr x9, [sp, #8]
  100000804:  e9 2f 00 f9                    str x9, [sp, #88]
  100000808:  e9 0b 40 f9                    ldr x9, [sp, #16]
  10000080c:  e9 33 00 f9                    str x9, [sp, #96]
  100000810:  e9 2f 40 f9                    ldr x9, [sp, #88]
  100000814:  e9 13 00 f9                    str x9, [sp, #32]
  100000818:  e9 33 40 f9                    ldr x9, [sp, #96]
  10000081c:  e9 17 00 f9                    str x9, [sp, #40]
.L1:
; block loop
  100000820:  e9 17 40 f9                    ldr x9, [sp, #40]
  100000824:  ea 0f 40 f9                    ldr x10, [sp, #24]
  100000828:  3f 01 0a eb                    cmp x9, x10
  10000082c:  e9 d7 9f 9a                    cset x9, gt
  100000830:  e9 1b 00 f9                    str x9, [sp, #48]
  100000834:  e9 1b 40 f9                    ldr x9, [sp, #48]
  100000838:  49 00 00 b5                    cbnz x9, .L2
  10000083c:  20 00 00 14                    b .L4
.L2:
; block write
  100000840:  e0 03 40 f9                    ldr x0, [sp]
  100000844:  e1 13 40 f9                    ldr x1, [sp, #32]
  100000848:  e2 17 40 f9                    ldr x2, [sp, #40]
  10000084c:  10 00 00 90                    adrp x16, __imp.libSystem.write@PAGE
  100000850:  10 02 40 f9                    ldr x16, [x16, __imp.libSystem.write@PAGEOFF]
  100000854:  00 02 3f d6                    blr x16
  100000858:  e0 1f 00 f9                    str x0, [sp, #56]
  10000085c:  e9 1f 40 f9                    ldr x9, [sp, #56]
  100000860:  ea 0f 40 f9                    ldr x10, [sp, #24]
  100000864:  3f 01 0a eb                    cmp x9, x10
  100000868:  e9 c7 9f 9a                    cset x9, le
  10000086c:  e9 23 00 f9                    str x9, [sp, #64]
  100000870:  e9 23 40 f9                    ldr x9, [sp, #64]
  100000874:  49 02 00 b5                    cbnz x9, .L4
.L3:
; block advance
  100000878:  e9 13 40 f9                    ldr x9, [sp, #32]
  10000087c:  ea 1f 40 f9                    ldr x10, [sp, #56]
  100000880:  29 01 0a 8b                    add x9, x9, x10
  100000884:  e9 27 00 f9                    str x9, [sp, #72]
  100000888:  e9 17 40 f9                    ldr x9, [sp, #40]
  10000088c:  ea 1f 40 f9                    ldr x10, [sp, #56]
  100000890:  29 01 0a cb                    sub x9, x9, x10
  100000894:  e9 2b 00 f9                    str x9, [sp, #80]
  100000898:  e9 27 40 f9                    ldr x9, [sp, #72]
  10000089c:  e9 2f 00 f9                    str x9, [sp, #88]
  1000008a0:  e9 2b 40 f9                    ldr x9, [sp, #80]
  1000008a4:  e9 33 00 f9                    str x9, [sp, #96]
  1000008a8:  e9 2f 40 f9                    ldr x9, [sp, #88]
  1000008ac:  e9 13 00 f9                    str x9, [sp, #32]
  1000008b0:  e9 33 40 f9                    ldr x9, [sp, #96]
  1000008b4:  e9 17 00 f9                    str x9, [sp, #40]
  1000008b8:  da ff ff 17                    b .L1
.L4:
; block done
  1000008bc:  bf 03 00 91                    add sp, x29, #0
  1000008c0:  fd 7b c1 a8                    ldp x29, x30, [sp], #16
  1000008c4:  c0 03 5f d6                    ret

