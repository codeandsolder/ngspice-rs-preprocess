#include <stdint.h>
#include <stddef.h>
#include <stdbool.h>
#ifndef MINOR
#define MINOR 4
#endif
#ifndef ROW
#define ROW 0
#endif
#ifndef FLAGS
#define FLAGS 4
#endif
typedef void (*Callback)(void);
typedef struct { const char *name, *units, *residual; uint32_t offsets[4]; bool flow; } Node;
typedef struct { uint32_t row, column, reactive, flags; } Jacobian;
typedef struct { const char **names; uint32_t aliases; const char *desc, *unit; uint32_t flags, length; } Param;
typedef struct { const char *name; uint32_t first, second; } Noise;
typedef struct { uint32_t first, second; } Pair;
typedef struct {
    const char *name;
    uint32_t nodes, terminals; const Node *node;
    uint32_t jacobians; const Jacobian *jacobian;
    uint32_t collapsible; const Pair *collapse; uint32_t collapse_offset;
    const Noise *noise; uint32_t noises;
    uint32_t params, instances, opvars; const Param *parameter;
    uint32_t mapping, jac_mapping, states, state_mapping, bound, instance_size, model_size;
    Callback callbacks[14];
#if MINOR == 4
    Callback given[2];
    uint32_t resistive_entries, reactive_entries;
    Callback write[2];
    uint32_t inputs; const Pair *input;
    Callback load_offset[2];
    const Pair *unknown_nature, *residual_nature;
    const uint32_t *noise_types;
    Callback noise_params;
    uint32_t module_flags;
#endif
} Descriptor;
static void callback(void) {}
static const Node nodes[] = {
    {"p", "V", "A", {0, UINT32_MAX, UINT32_MAX, UINT32_MAX}, false},
    {"n", "V", "A", {8, UINT32_MAX, UINT32_MAX, UINT32_MAX}, false},
};
static const Jacobian jac[] = {{ROW, 1, UINT32_MAX, FLAGS}};
static const char *res_names[] = {"resistance", "r"};
static const char *int_names[] = {"mode"};
static const char *arr_names[] = {"labels"};
static const char *out_names[] = {"current"};
static const Param params[] = {
    {res_names, 1, "resistance", "ohm", 1u<<30, 0},
    {int_names, 0, "mode", "", 1, 0},
    {arr_names, 0, "labels", "", 2, 2},
    {out_names, 0, "output", "A", 2u<<30, 0},
};
static const Noise noise[] = {{"thermal", 0, 1}};
static const uint32_t noise_types[] = {0};
static const Pair natures[] = {{0,0},{0,0}};
const uint32_t OSDI_VERSION_MAJOR = 0;
const uint32_t OSDI_VERSION_MINOR = MINOR;
const uint32_t OSDI_NUM_DESCRIPTORS = 1;
#if MINOR == 4
const uint32_t OSDI_DESCRIPTOR_SIZE = sizeof(Descriptor);
#endif
const Descriptor OSDI_DESCRIPTORS[] = {{
    "test", 2, 2, nodes, 1, jac, 0, 0, 0, noise, 1, 3, 1, 1, params,
    16, 24, 0, 0, UINT32_MAX, 48, 16,
    {callback,callback,callback,callback,callback,callback,callback,callback,
     callback,callback,callback,callback,callback,callback},
#if MINOR == 4
    {callback,callback}, 1, 0, {callback,callback}, 0, 0,
    {callback,callback}, natures, natures, noise_types, callback, 0,
#endif
}};
__attribute__((constructor)) static void never_execute(void) { __builtin_trap(); }
