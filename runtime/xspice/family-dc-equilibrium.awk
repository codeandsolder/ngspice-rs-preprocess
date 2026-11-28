# Independent current-balance reference for the measured 5 V HC DAC law.
# Voltages (including the smoothing half-width) scale; conductances do not.
function hc_branch(v, g, knee, width, d) {
    if (v <= knee-width) return g*v
    if (v >= knee+width) return g*knee
    d=v-knee+width
    return g*(v-d*d/(4*width))
}
function hc_bus(supply, n_high, n_low, load, lo, hi, mid, f, i, scale) {
    scale=supply/5
    lo=0
    hi=supply
    for(i=0;i<60;i++) {
        mid=(lo+hi)/2
        f=n_high*hc_branch(supply-mid,.024,2.25*scale,.5*scale)
        f-=n_low*hc_branch(mid,.034,2*scale,.5*scale)+load*mid
        if(f>0) lo=mid
        else hi=mid
    }
    return (lo+hi)/2
}

# All original-source strength probes share these fixed 3.3 V HC output
# parameters, 1.2 V bias, 1 kohm and 1 mS resistive drive. The vendor
# twelve-state forced-current matrix establishes that undetermined
# drive floats and resistive UNKNOWN drives the midpoint.
function floating_drive(code) { return substr(code,2,1) ~ /^[zu]$/ }
function biased_hc_probe(code, level, strength) {
    level=substr(code,1,1)
    strength=substr(code,2,1)
    if(floating_drive(code)) return 1.2
    if(strength=="r") return level=="0"?.6:level=="1"?2.25:1.425
    if(level=="U") return .433333333
    return level=="0"?.034285714:3.21542623
}
