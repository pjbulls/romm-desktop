/* Prints sizeof/offsetof of the sigil.h structs that sigil-sys transcribes, as Rust `assert_eq!`
 * lines for src/lib.rs. Run through scripts/check-sigil-layout.sh. */
#include <stddef.h>
#include <stdio.h>
#include "sigil.h"

#define SZ(t)       printf("        assert_eq!(size_of::<%s>(), %zu);\n", "sigil_" #t, sizeof(sigil_##t))
#define OFF(t, f)   printf("        assert_eq!(offset_of!(sigil_%s, %s), %zu);\n", #t, #f, offsetof(sigil_##t, f))

int main(void) {
    printf("// result\n");
    SZ(result); OFF(result, platform); OFF(result, features); OFF(result, n64_header); OFF(result, n64_md5_n64);
    printf("// save_request\n");
    SZ(save_request); OFF(save_request, result); OFF(save_request, options); OFF(save_request, listing);
    OFF(save_request, open); OFF(save_request, root_path); OFF(save_request, profile);
    printf("// save_unit\n");
    SZ(save_unit); OFF(save_unit, shape); OFF(save_unit, members); OFF(save_unit, unkeyed); OFF(save_unit, artifact);
    OFF(save_unit, content_hash); OFF(save_unit, identity_hash); OFF(save_unit, alternates);
    printf("// sync_request\n");
    SZ(sync_request); OFF(sync_request, save); OFF(sync_request, game_ids); OFF(sync_request, mode);
    OFF(sync_request, state); OFF(sync_request, overwrite_local); OFF(sync_request, write); OFF(sync_request, claimed);
    OFF(sync_request, remove); OFF(sync_request, companions); OFF(sync_request, repair);
    printf("// sync_result\n");
    SZ(sync_result); OFF(sync_result, artifact); OFF(sync_result, shape); OFF(sync_result, data);
    OFF(sync_result, content_hash); OFF(sync_result, changed); OFF(sync_result, state); OFF(sync_result, holding);
    OFF(sync_result, unowned); OFF(sync_result, restore_again); OFF(sync_result, companions); OFF(sync_result, problem);
    OFF(sync_result, blocks_short); OFF(sync_result, profiles); OFF(sync_result, profile); OFF(sync_result, alternates);
    OFF(sync_result, hardcore_marker); OFF(sync_result, unowned_changed);
    printf("// small ones\n");
    SZ(save_member); SZ(save_profile); SZ(save_option); SZ(save_alternate); SZ(sync_companion);
    SZ(sync_companion_result); SZ(io); SZ(support); SZ(options);
    return 0;
}
