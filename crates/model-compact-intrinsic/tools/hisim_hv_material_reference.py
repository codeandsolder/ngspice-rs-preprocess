"""Independent HiSIM-HV channel impurity profile, pocket overlap and ddlt.

Usage: uv run --no-project python hisim_hv_material_reference.py NGSPICE_CHECKOUT OUTPUT
Extract the native DFM, surface impurity, pocket overlap, and Vdseff
geometry preparation sections from the pinned ngspice 47 source.
"""
from itertools import product
from pathlib import Path
import math
import re
import subprocess
import sys
import tempfile

REVISION="972dbf45db1e47d18bc4fda5d8bb829aff2d0e05"


def program(root, version):
    prefix="HSMHV" if version==1 else "HSMHV2"
    base="hsmhv" if version==1 else "hsmhv2"
    path=f"src/spicelib/devices/hisimhv{version}/{base}temp.c"
    src=subprocess.check_output(["git","-C",str(root),"show",f"{REVISION}:{path}"],text=True)
    start=src.index(f"Npext = pParam->{prefix}_npext")
    initial=src[start:src.index("/* Phonon Scattering",start)]
    start=src.index("/* Surface impurity profile */")
    profile=src[start:src.index("/* depletion MOS",start) if version==2 else src.index("/* costi0",start)]
    start=src.index("/* Vdseff */",src.index("/* Body resistance */"))
    drain=src[start:src.index("/* Isub */",start)]
    def struct(name,fields,integer_fields=()):
        return "typedef struct { "+" ".join(f"double {prefix}_{p};" for p in fields)+" "+" ".join(f"int {prefix}_{p};" for p in integer_fields)+" } "+name+";"
    param=struct("Param",("npext","mueph1","nsubp","nsubc","nsubpsti1","nsubpsti2","nsubpsti3"))
    model=struct("Model",("mphdfm","nsubp0","nsubwp","nsubcw","nsubcwp","lp","lpext","ddltslp","ddltict","ddltmax"),("codfm","coddlt"))
    instance=struct("Instance",("mueph1","nsubp","nsubc","nsubcdfm","nsub","qnsub","qnsub_esi","2qnsub_esi","ptovr0","ddlt"),("nsubcdfm_Given",))
    return f'''#include <math.h>
#include <stdio.h>
#define C_QE 1.6021918e-19
#define C_ESI 1.034943e-10
#define Fn_Max fmax
#define RANGECHECK(...) ((void)0)
{param}
{model}
{instance}
int main(void){{ double x[6];while(1){{
 for(int i=0;i<6;i++)if(scanf("%lf",&x[i])!=1)return 0;
 double Lgate=x[0]*1e-6,LG=x[0],WG=x[1],nf=x[2],layout=x[3],dfm=x[4],small=1e-25;
 Param pp={{0}};Model mm={{0}};Instance ii={{0}},imks={{0}};
 Param*pParam=&pp;Model*model=&mm;Instance*here=&ii,*hereMKS=&imks;
 pp.{prefix}_npext=3e23;pp.{prefix}_nsubc=1e22;pp.{prefix}_nsubp=5e23;pp.{prefix}_mueph1=100.;
 pp.{prefix}_nsubpsti1=8e-7;pp.{prefix}_nsubpsti2=.25;pp.{prefix}_nsubpsti3=1.2;
 mm.{prefix}_codfm=1;mm.{prefix}_mphdfm=0.;ii.{prefix}_nsubcdfm_Given=(int)dfm;ii.{prefix}_nsubcdfm=2e16;imks.{prefix}_nsubcdfm=2e22;
 mm.{prefix}_nsubp0=.2;mm.{prefix}_nsubwp=.7;mm.{prefix}_nsubcw=.3;mm.{prefix}_nsubcwp=.8;mm.{prefix}_lp=.4e-6;mm.{prefix}_lpext=.15e-6;
 mm.{prefix}_ddltslp=.7;mm.{prefix}_ddltict=.8;mm.{prefix}_ddltmax=5.;mm.{prefix}_coddlt=(int)x[5];
 double Lod_half_ref=2./(1./(.5e-6+.5*Lgate)+1./(.4e-6+.5*Lgate)),Lod_half=0.;
 if(layout){{ double sum=0.;for(int i=0;i<nf;i++)sum+=1./(.2e-6+.5*Lgate+i*(.1e-6+Lgate))+1./(.3e-6+.5*Lgate+i*(.1e-6+Lgate));Lod_half=2.*nf/sum; }}
 double Npext=0.,Nsubpp=0.,Nsubps=0.,Nsub=0.,Nsubb=0.,q_Nsub=0.,T1=0.,T2=0.,T3=0.;
 {initial}
 {profile}
 {drain}
 printf("%.17g %.17g %.17g %.17g %.17g %.17g %.17g\\n",Nsub,here->{prefix}_nsubc,here->{prefix}_nsubp,Nsubps,Npext,here->{prefix}_ptovr0,here->{prefix}_ddlt);
 }}}}
'''


def main():
    root,out=Path(sys.argv[1]),Path(sys.argv[2])
    with tempfile.TemporaryDirectory(prefix="hisim-material-oracle-") as temp,out.open("w") as f:
        f.write("# family L_um W_per_finger_um NF layout dfm coddlt; effective channel, bulk, pocket, corrected pocket, extension doping in m^-3, overlap log, ddlt\n")
        for version in (1,2):
            cases=list(product((.3,.7,1.3,4.),(2.5,8.),(1.,3.),(0,1),(0,1),(0,) if version==1 else (0,1)))
            c,exe=Path(temp)/"oracle.c",Path(temp)/"oracle"
            c.write_text(program(root,version))
            subprocess.run(["sccache","cc","-std=c11","-O2",str(c),"-lm","-o",str(exe)],check=True)
            result=subprocess.run([str(exe)],input="".join(" ".join(map(str,row))+"\n" for row in cases),text=True,capture_output=True,check=True)
            rows=result.stdout.splitlines()
            assert len(rows)==len(cases)
            for case,row in zip(cases,rows):
                assert all(math.isfinite(float(x)) for x in row.split()),(case,row)
                f.write(" ".join(map(str,(version,*case)))+" "+row+"\n")

if __name__=="__main__":main()
