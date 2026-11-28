"""Extract exact HiSIM-HV non-edge gate currents from pinned ngspice 47 C.

Usage: uv run --no-project python hisim_hv_gate_reference.py NGSPICE_CHECKOUT OUTPUT
Evaluate physical values and independently differentiate the actual C values.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

REVISION = "972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"
MODEL = {"gleak1":50.,"gleak2":1e6,"gleak3":.06,"gleak4":4.,"gleak5":7500.,"gleak6":.25,"gleak7":1e-6,"glkb1":5e-16,"glkb2":1.,"glkb3":.1,"fn1":50.,"fn2":1e6,"fn3":0.,"fvbs":.012,"vzadd0":.01}
GEOMETRIES = [(10.,50.,1.,1.),(3.,24.,3.,2.3)]
# The independently provided channel state is affine in these trial coordinates.
# Each row represents Vgsz, Vdsz, Vbsz, Ps0z, dVth, dPpg, SC+LP shift,
# Ey, Qiu, cnst0. Values/derivatives exercise all inputs of the native branch.
STATE_GRADIENTS = [(.94,.01,.03,-.00015),(0.,.86,.02,0.),(0.,-.02,.93,0.),(.08,.02,.1,-.0001),(.025,.013,-.04,.00025),(.01,.001,.005,.00005),(.04,.015,-.07,.0002),(.05,1.,.04,-.001),(1e-4,2e-5,-3e-5,1e-7),(0.,0.,0.,1e-8)]
BASE = (.02,.04,-.02,.35,.10,.03,.20,2.,.004,.003)
TOX = 8e-9
VFB = -.8
COX = 3.453133e-11/TOX
EG0,EGIG,BGTMP1,BGTMP2,IGTEMP2,IGTEMP3 = 1.2,.06,9.025e-5,1e-7,110.,1300.
TNOM=300.15


def program(root,version):
    prefix="HSMHV" if version==1 else "HSMHV2"
    base="hsmhv" if version==1 else "hsmhv2"
    path=f"src/spicelib/devices/hisimhv{version}"
    def source(name):
        return subprocess.check_output(["git","-C",str(root),"show",f"{REVISION}:{path}/{name}"],text=True)
    evaluate,setup=source(f"{base}eval.c"),source(f"{base}set.c")
    start=evaluate.index("#define Fn_SZ(");end=evaluate.index("\n\n",start)
    macro=evaluate[start:end]
    start=evaluate.index("    /* Igate */",evaluate.index("* Igate : Gate current induced by tunneling."))
    end=evaluate.index("    /* Igs */",start)
    inversion=evaluate[start:end]
    start=evaluate.index("    /* Igb */",end)
    end=evaluate.index("      Igb -= Ifn",start)
    body=evaluate[start:end]
    pfields=("gleak1","gleak2","gleak3","gleak6","glkb1","glkb2","fn1","fn2","fn3","fvbs")
    model_fields=("glkb3","vzadd0")
    mks_fields=("gleak4","gleak5","gleak7")
    def struct(name,fields):return "typedef struct { double "+",".join(prefix+"_"+p for p in fields)+"; } "+name+";"
    def initialize(name,fields):return "\n".join(f"{name}.{prefix}_{p}={MODEL[p]:.17g};" for p in fields)
    conversion=[]
    for storage,fields in (("pParam",("gleak2","glkb2","fn2")),("modelMKS",mks_fields)):
        for p in fields:
            line=next(line for line in setup.splitlines() if f"{storage}->{prefix}_{p} " in line and "C_m2cm" in line)
            if storage=="modelMKS":line=line.replace(f"model->{prefix}_{p}",f"modelMKS->{prefix}_{p}")
            conversion.append(line)
    vars_="T0 T1 T2 T3 T4 T5 T6 T7 T9 T10 TX TMF2 Psdlz Etun Igate Igb Ifn Eg12 Eg32".split()
    # The source partial assignments are included verbatim; state derivatives
    # are irrelevant to these value outputs, so initialize them to zero.
    derivs=set(re.findall(r"\b\w+_d(?:V\w+|T)\b",inversion+body))
    decl="\n".join(f"double {name}=0.;" for name in sorted(set(vars_)|derivs))
    return f'''#include <math.h>
#include <stdio.h>
#include <float.h>
#define C_m2cm 100.
#define C_m2cm_p2 10000.
#define C_QE 1.6021918e-19
#define EXP_THR 34.
{macro}
{struct('Param',pfields)}
{struct('Model',model_fields)}
{struct('MKS',mks_fields)}
{struct('Instance',('weff_nf','lgate','sqrt_eg','eg'))}
int main(void) {{ double x[22]; while (1) {{
 for (int i=0;i<22;i++) if (scanf("%lf",&x[i])!=1) return 0;
 Param par={{0}}; Model md={{0}}; MKS mks={{0}}; Instance inst={{0}};
 Param *pParam=&par; Model *model=&md; MKS *modelMKS=&mks; Instance *here=&inst;
 {initialize('par',pfields)}
 {initialize('md',model_fields)}
 {initialize('mks',mks_fields)}
 {chr(10).join(conversion)}
 int enabled=(int)x[0],flg_noqi=(int)x[1]; double Leff=x[2],Mfactor=x[4],Vgs=x[5],Vbs=x[6],Vfb=x[7],Tox0=x[8],Cox0=x[9];
 here->{prefix}_weff_nf=x[3];here->{prefix}_lgate=Leff;here->{prefix}_eg=x[10];here->{prefix}_sqrt_eg=sqrt(x[10]);
 double Egp12=sqrt(x[11]),Egp32=x[11]*Egp12;
 double Vgsz=x[12],Vdsz=x[13],Vbsz=x[14],Ps0z=x[15],dVth=x[16],dPpg=x[17],dVthSC=x[18],dVthLP=0.,Ey=x[19],Qiu=x[20],cnst0=x[21];
 const double epsm10=10.*DBL_EPSILON,igate_dlt=1.,small=1e-25,cclmmdf=.1,VgVt_small=1e-12,c_exp_2=7.38905609893065;
 {decl}
 if (enabled) {{ {inversion} {body} }}
 printf("%.17g %.17g %.17g\\n", Mfactor*Igate,Mfactor*Igb,Mfactor*Ifn);
 }} }}
'''


def raw(case, trial):
    enabled,no_inv,geometry=case
    length,width,nf,m=geometry
    gate,drain,body,temp=trial
    nominal_gap=EG0-TNOM*(90.25e-6+TNOM*1e-7)
    gap=nominal_gap-BGTMP1*(temp-TNOM)-BGTMP2*(temp**2-TNOM**2)
    ig_gap=nominal_gap+EGIG+IGTEMP2*(1/temp-1/TNOM)+IGTEMP3*(1/temp**2-1/TNOM**2)
    state=[base+sum(a*b for a,b in zip(grad,(gate,drain,body,temp-TNOM))) for base,grad in zip(BASE,STATE_GRADIENTS)]
    return [enabled,no_inv,length*1e-6,width*1e-6,m,gate,body,VFB,TOX,COX,gap,ig_gap,*state]


def main():
    root=Path(sys.argv[1]);out=Path(sys.argv[2])
    cases=list(product((0,1),(0,1),GEOMETRIES,((-2.,.6,-.2,260.),(-.6,.02,-.5,300.15),(.4,1.,-.1,350.),(1.5,3.,-.3,260.),(3.,.7,-.1,350.))))
    inputs=[]
    for enabled,no_inv,geometry,trial in cases:
        case=enabled,no_inv,geometry
        inputs.append(raw(case,trial))
        for axis in range(4):
            step=1e-3 if axis==3 else 1e-5
            for sign in (1,-1):
                values=list(trial);values[axis]+=step*sign
                inputs.append(raw(case,values))
    with tempfile.TemporaryDirectory(prefix="hisim-gate-oracle-") as temp,out.open("w") as output:
        output.write("# ngspice 47: family enabled no_inv L_um W_um NF M Vgs Vds Vbs T, then inversion/direct-body/FN values and four finite-difference partials each.\n")
        for version in (1,2):
            source,binary=Path(temp)/"oracle.c",Path(temp)/"oracle"
            source.write_text(program(root,version))
            subprocess.run(["sccache","cc","-std=c11","-O2",str(source),"-lm","-o",str(binary)],check=True)
            result=subprocess.run([str(binary)],input="".join(" ".join(map(str,row))+"\n" for row in inputs),text=True,capture_output=True,check=True)
            rows=[list(map(float,row.split())) for row in result.stdout.splitlines()]
            assert len(rows)==len(inputs)
            for i,(enabled,no_inv,geometry,trial) in enumerate(cases):
                center=rows[9*i];partials=[[ (rows[9*i+1+2*axis][side]-rows[9*i+2+2*axis][side])/(2*(1e-3 if axis==3 else 1e-5)) for axis in range(4)] for side in range(3)]
                values=[v for side in range(3) for v in (center[side],*partials[side])]
                assert all(math.isfinite(v) for v in values),(version,i,values)
                output.write(" ".join(map(str,(version,enabled,no_inv,*geometry,*trial,*values)))+"\n")

if __name__=="__main__":main()
