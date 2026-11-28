"""Generate GIDL/GISL reference from the pinned ngspice 47 C evaluator.

Usage: uv run --no-project python hisim_hv_gidl_reference.py NGSPICE_CHECKOUT OUTPUT
The prepared geometry and raw coefficients are explicit in the fixture schema.
"""
from itertools import product
from pathlib import Path
import subprocess
import sys
import tempfile

REVISION = "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
MODEL = (2.0, 3e7, .9, .15, .2, 8e-9, 1.2, .06, 110., 1300.)
GEOMETRIES = [(10., 50., 1., 1.), (3., 24., 3., 2.3)]
# Raw C coefficients: include nontrivial L/W/P contributions on both GIDL laws.
BINS = [(0.8, 1.2, .3), (2e6, 3e6, 1e6)]


def program(root, version):
    prefix = "HSMHV" if version == 1 else "HSMHV2"
    base = "hsmhv" if version == 1 else "hsmhv2"
    path = f"src/spicelib/devices/hisimhv{version}"
    def source(name):
        return subprocess.check_output(["git", "-C", str(root), "show", f"{REVISION}:{path}/{name}"], text=True)
    evaluate = source(f"{base}eval.c")
    setup = source(f"{base}set.c")
    macro_start = evaluate.index("#define Fn_SZ(")
    macro_end = evaluate.index("\n\n", macro_start)
    start = evaluate.index("  if ( model->" + prefix + "_cogidl != 0 )", evaluate.index("* Vdsp : Vds modification for GIDL/GISL"))
    end = evaluate.index("* End of PART-2.", start)
    end = evaluate.rindex("  /*", start, end)
    block = evaluate[start:end]
    conversion = "\n".join(next(line for line in setup.splitlines() if f"pParam->{prefix}_gidl{i} " in line and "C_m2cm" in line) for i in (1, 2))
    variables = "T0 T1 T2 T3 T4 T5 T7 T0_dT T1_dT T2_dT TMF2 Vdsp Vdsp_dVd E1 E1_dVb E1_dVd E1_dVg E1_dT Egidl Egidl_dVb Egidl_dVd Egidl_dVg Egidl_dT Egisl Egisl_dVb Egisl_dVd Egisl_dVg Egisl_dT Vdb Vsb Igidl Igidl_dVgs Igidl_dVds Igidl_dVbs Igidl_dT Igisl Igisl_dVgs Igisl_dVds Igisl_dVbs Igisl_dT".split()
    decl = "\n".join(f"double {name}=0.;" for name in variables)
    outputs = ["Igidl", "Igidl_dVgs", "Igidl_dVds", "Igidl_dVbs", "Igidl_dT", "Igisl", "Igisl_dVgs", "Igisl_dVds", "Igisl_dVbs", "Igisl_dT"]
    return f'''#include <math.h>
#include <stdio.h>
#define C_m2cm 100.
#define C_m2cm_p1o2 10.
#define C_QE 1.6021918e-19
#define C_gidl_delta .5
#define EXP_THR 34.
{evaluate[macro_start:macro_end]}
typedef struct {{int {prefix}_cogidl; double {prefix}_gidl3,{prefix}_gidl4,{prefix}_gidl5;}} Model;
typedef struct {{double {prefix}_gidl1,{prefix}_gidl2;}} Param;
typedef struct {{double {prefix}_weff_nf;}} Instance;
int main(void) {{ double x[20]; while (1) {{
 for (int i=0;i<20;i++) if (scanf("%lf", &x[i])!=1) return 0;
 Model md={{(int)x[0],x[3],x[4],x[5]}}; Param par={{x[1],x[2]}}; Instance inst={{x[7]}};
 Model *model=&md; Param *pParam=&par; Instance *here=&inst;
 double Tox0=x[6], Mfactor=x[8], Vgs=x[9], Vds=x[10], Vbs=x[11];
 double dVthSC=x[12], dVthLP=0., dVthSC_dVg=x[13], dVthSC_dVd=x[14], dVthSC_dVb=x[15], dVthSC_dT=x[16];
 double dVthLP_dVg=0.,dVthLP_dVd=0.,dVthLP_dVb=0.,dVthLP_dT=0.;
 double Egp12=sqrt(x[17]), Egp32=x[17]*Egp12, Egp12_dT=.5*x[18]/Egp12, Egp32_dT=1.5*x[18]*Egp12;
 const double small=1e-25,eef_dlt=1.,gidla=100.,gidlvds_dlt=1e-5;
 {decl}
 {conversion}
 {block}
 printf("{' '.join(['%.17g']*10)}\\n", {', '.join('Mfactor*'+o for o in outputs)});
 }} }}
'''


def case_inputs():
    first, second, third, fourth, fifth, oxide, eg0, egig, igtemp2, igtemp3 = MODEL
    for enabled, geometry, gate, drain, body, temperature in product((0,1), GEOMETRIES, (-4., -1.2, .5), (0., .003, .03, 1., 15.), (-1., -.05, .01), (260., 350.)):
        length, width, nf, multiplier = geometry
        wbin = width/nf
        def binned(base, coeffs):
            l, w, p = coeffs
            return base + l/length + w/wbin + p/(length*wbin)
        gradients = (.04, .015, -.07, .0002)
        shift = .25 + sum(a*b for a,b in zip(gradients, (gate,drain,body,temperature-300.15)))
        nominal = 300.15
        bandgap_nominal = eg0 - nominal * (90.25e-6 + nominal*1e-7)
        inv = 1/temperature
        inv_nominal = 1/nominal
        bandgap = bandgap_nominal+egig+igtemp2*(inv-inv_nominal)+igtemp3*(inv**2-inv_nominal**2)
        bandgap_slope = -igtemp2*inv**2-2*igtemp3*inv**3
        raw = [enabled,binned(first,BINS[0]),binned(second,BINS[1]),third,fourth,fifth,oxide,width*1e-6,multiplier,gate,drain,body,shift,*gradients,bandgap,bandgap_slope,temperature]
        yield [enabled,*geometry,gate,drain,body,temperature,shift,*gradients],raw


def main():
    root = Path(sys.argv[1])
    subprocess.run(["git", "-C", str(root), "cat-file", "-e", REVISION + "^{commit}"], check=True)
    cases = list(case_inputs())
    with tempfile.TemporaryDirectory(prefix="hisim-gidl-oracle-") as temp, Path(sys.argv[2]).open("w") as output:
        output.write("# ngspice 47 exact GIDL/GISL C evaluator, normalized bias. family enabled L_um W_um NF M Vgs Vds Vbs T shift shift_Vgs shift_Vds shift_Vbs shift_T then Id and Is plus four partials each.\n")
        for version in (1,2):
            source, binary = Path(temp)/"oracle.c", Path(temp)/"oracle"
            source.write_text(program(root,version))
            subprocess.run(["sccache","cc","-std=c11","-O2",str(source),"-lm","-o",str(binary)],check=True)
            result = subprocess.run([str(binary)],input="".join(" ".join(map(str,raw))+"\n" for _,raw in cases),text=True,capture_output=True,check=True)
            lines = result.stdout.splitlines()
            assert len(lines)==len(cases), (len(lines),len(cases))
            for (inputs,_),line in zip(cases,lines):
                output.write(" ".join(map(str,(version,*inputs)))+" "+line+"\n")


if __name__ == "__main__":
    main()
